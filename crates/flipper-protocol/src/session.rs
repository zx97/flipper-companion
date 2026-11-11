//! Asynchronous RPC session over a serial stream.
//!
//! The session is an actor:
//!
//! ```text
//! caller ──command──▶ session actor ──frame──▶ writer task ──▶ serial port
//! caller ◀──reply──── session actor ◀──frame── reader stream ◀── serial port
//! ```
//!
//! Requests are correlated by `command_id`.  Multi-part responses
//! (`has_next == true`) and multi-request exchanges (storage writes) are
//! handled centrally, so typed helpers stay short and declarative.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use flipper_companion_core::{DeviceInfo, EntryKind, ProtobufVersion, StorageEntry, StorageInfo};
use futures::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{broadcast, mpsc, oneshot},
    time::timeout,
};
use tokio_util::codec::Framed;

use crate::{
    codec::FrameCodec,
    error::{ProtocolError, status_label},
    pb, pb_app, pb_desktop, pb_gpio, pb_gui, pb_property, pb_storage, pb_system,
};

/// Payload size used by the official client for `Storage_write`.
pub const STORAGE_CHUNK_SIZE: usize = 1024;

/// Default timeout applied to a complete request/response exchange.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Events pushed by the device outside a request/response exchange.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A frame with `command_id == 0` (screen frames, desktop status, ...).
    Broadcast(pb::Main),
    /// The transport or the actor stopped.
    Closed(Option<String>),
}

/// A connected, ready-to-use RPC session.
pub struct RpcSession {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<SessionEvent>,
    request_timeout: Duration,
}

enum Command {
    Request {
        outgoing: VecDeque<Outgoing>,
        reply: oneshot::Sender<Result<Vec<pb::Main>, ProtocolError>>,
    },
    Shutdown,
}

struct Outgoing {
    content: pb::main::Content,
    has_next: bool,
}

struct Pending {
    responses: Vec<pb::Main>,
    reply: oneshot::Sender<Result<Vec<pb::Main>, ProtocolError>>,
}

impl RpcSession {
    /// Starts a session over an already-open transport.
    pub fn spawn<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (commands, command_rx) = mpsc::channel(32);
        let (events, _) = broadcast::channel(64);

        tokio::spawn(run(stream, command_rx, events.clone()));

        Self {
            commands,
            events,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Overrides the timeout applied to each request.
    #[must_use]
    pub fn with_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    /// Subscribes to unsolicited device events (screen frames, ...).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// Sends one request and returns every response part.
    pub async fn call(&self, content: pb::main::Content) -> Result<Vec<pb::Main>, ProtocolError> {
        self.exchange(vec![(content, false)]).await
    }

    /// Sends a request that expects exactly one response part.
    pub async fn call_one(&self, content: pb::main::Content) -> Result<pb::Main, ProtocolError> {
        let mut responses = self.call(content).await?;
        if responses.is_empty() {
            Err(ProtocolError::UnexpectedResponse)
        } else {
            Ok(responses.remove(0))
        }
    }

    /// Sends several requests under one `command_id`, back-to-back.
    ///
    /// The Flipper firmware answers only the **last** frame of a continuous
    /// command (`has_next == true` for the preceding ones), so the whole
    /// sequence is written before waiting for the final response.  This is how
    /// `Storage_write` transfers a file in 1 KiB chunks.
    pub async fn exchange(
        &self,
        outgoing: Vec<(pb::main::Content, bool)>,
    ) -> Result<Vec<pb::Main>, ProtocolError> {
        let (reply, response) = oneshot::channel();

        let command = Command::Request {
            outgoing: outgoing
                .into_iter()
                .map(|(content, has_next)| Outgoing { content, has_next })
                .collect::<VecDeque<_>>(),
            reply,
        };

        self.commands
            .send(command)
            .await
            .map_err(|_| ProtocolError::Closed(String::from("session task stopped")))?;

        match timeout(self.request_timeout, response).await {
            Err(_) => {
                tracing::warn!(
                    "session: request timed out after {:?}",
                    self.request_timeout
                );
                Err(ProtocolError::Timeout(self.request_timeout))
            }
            Ok(Err(_)) => Err(ProtocolError::Closed(String::from(
                "response channel was dropped",
            ))),
            Ok(Ok(result)) => result,
        }
    }

    /// Stops the device-side RPC service and the local actor.
    ///
    /// The firmware closes the session instead of acknowledging
    /// `Stop_session`, so the request is fire-and-forget: waiting for a reply
    /// would block for the full request timeout on every disconnect.
    pub async fn stop(&self) {
        let (reply, response) = oneshot::channel();

        let command = Command::Request {
            outgoing: vec![Outgoing {
                content: pb::main::Content::StopSession(pb::StopSession {}),
                has_next: false,
            }]
            .into(),
            reply,
        };

        if self.commands.send(command).await.is_ok() {
            let _ = timeout(Duration::from_millis(200), response).await;
        }

        tracing::debug!("session: closed");

        let _ = self.commands.send(Command::Shutdown).await;
    }

    /// Sends a ping and returns the echoed payload.
    pub async fn ping(&self, data: Vec<u8>) -> Result<Vec<u8>, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::SystemPingRequest(
                pb_system::PingRequest { data },
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::SystemPingResponse(response)) => Ok(response.data),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Returns the protobuf protocol revision implemented by the firmware.
    pub async fn protobuf_version(&self) -> Result<ProtobufVersion, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::SystemProtobufVersionRequest(
                pb_system::ProtobufVersionRequest {},
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::SystemProtobufVersionResponse(response)) => {
                Ok(ProtobufVersion::new(response.major, response.minor))
            }
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Reads the modern `devinfo` property set.
    pub async fn device_info(&self) -> Result<DeviceInfo, ProtocolError> {
        let properties = self.property_get("devinfo").await?;
        Ok(DeviceInfo::from_properties(&properties)?)
    }

    /// Reads the legacy `system_device_info` key/value set.
    pub async fn device_info_legacy(&self) -> Result<DeviceInfo, ProtocolError> {
        let responses = self
            .call(pb::main::Content::SystemDeviceInfoRequest(
                pb_system::DeviceInfoRequest {},
            ))
            .await?;

        let mut properties = Vec::new();

        for response in responses {
            match response.content {
                Some(pb::main::Content::SystemDeviceInfoResponse(response)) => {
                    properties.push((response.key, response.value));
                }
                Some(pb::main::Content::Empty(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(DeviceInfo::from_legacy_properties(&properties)?)
    }

    /// Reads all key/value pairs of a device property.
    pub async fn property_get(&self, key: &str) -> Result<Vec<(String, String)>, ProtocolError> {
        let responses = self
            .call(pb::main::Content::PropertyGetRequest(
                pb_property::GetRequest {
                    key: key.to_owned(),
                },
            ))
            .await?;

        let mut properties = Vec::new();

        for response in responses {
            match response.content {
                Some(pb::main::Content::PropertyGetResponse(response)) => {
                    properties.push((response.key, response.value));
                }
                Some(pb::main::Content::Empty(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(properties)
    }

    /// Lists a device directory.
    pub async fn storage_list(&self, path: &str) -> Result<Vec<StorageEntry>, ProtocolError> {
        let responses = self
            .call(pb::main::Content::StorageListRequest(
                pb_storage::ListRequest {
                    path: path.to_owned(),
                    include_md5: false,
                    filter_max_size: 0,
                },
            ))
            .await?;

        let mut entries = Vec::new();

        for response in responses {
            match response.content {
                Some(pb::main::Content::StorageListResponse(response)) => {
                    entries.extend(response.file.into_iter().map(entry_from_file));
                }
                Some(pb::main::Content::Empty(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(entries)
    }

    /// Returns capacity information for a mount point (usually `/ext`).
    pub async fn storage_info(&self, path: &str) -> Result<StorageInfo, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::StorageInfoRequest(
                pb_storage::InfoRequest {
                    path: path.to_owned(),
                },
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::StorageInfoResponse(response)) => {
                Ok(StorageInfo::new(response.total_space, response.free_space))
            }
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Returns metadata for a single path, or `None` when it does not exist.
    pub async fn storage_stat(&self, path: &str) -> Result<Option<StorageEntry>, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::StorageStatRequest(
                pb_storage::StatRequest {
                    path: path.to_owned(),
                },
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::StorageStatResponse(response)) => {
                Ok(response.file.map(|file| {
                    let mut entry = entry_from_file(file);

                    // Some firmware revisions return an empty name in a stat
                    // response; fall back to the requested path.
                    if entry.name.is_empty() {
                        entry.name = path.rsplit('/').next().unwrap_or(path).to_owned();
                    }

                    entry
                }))
            }
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Reads a remote file into memory.
    pub async fn storage_read(&self, path: &str) -> Result<Vec<u8>, ProtocolError> {
        let responses = self
            .call(pb::main::Content::StorageReadRequest(
                pb_storage::ReadRequest {
                    path: path.to_owned(),
                },
            ))
            .await?;

        let mut data = Vec::new();

        for response in responses {
            match response.content {
                Some(pb::main::Content::StorageReadResponse(response)) => {
                    if let Some(file) = response.file {
                        data.extend_from_slice(&file.data);
                    }
                }
                Some(pb::main::Content::Empty(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(data)
    }

    /// Writes a remote file, splitting it into the firmware's 1 KiB chunks.
    pub async fn storage_write(&self, path: &str, data: &[u8]) -> Result<(), ProtocolError> {
        let chunks: Vec<&[u8]> = if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(STORAGE_CHUNK_SIZE).collect()
        };

        let total = chunks.len();
        let outgoing = chunks
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| {
                let file = pb_storage::File {
                    data: chunk.to_vec(),
                    ..pb_storage::File::default()
                };

                (
                    pb::main::Content::StorageWriteRequest(pb_storage::WriteRequest {
                        path: path.to_owned(),
                        file: Some(file),
                    }),
                    index + 1 < total,
                )
            })
            .collect();

        let responses = self.exchange(outgoing).await?;

        for response in responses {
            match response.content {
                Some(pb::main::Content::Empty(_) | pb::main::Content::SystemPingResponse(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(())
    }

    /// Creates a directory.
    pub async fn storage_mkdir(&self, path: &str) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::StorageMkdirRequest(
            pb_storage::MkdirRequest {
                path: path.to_owned(),
            },
        ))
        .await
    }

    /// Deletes a file or directory.
    pub async fn storage_remove(&self, path: &str, recursive: bool) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::StorageDeleteRequest(
            pb_storage::DeleteRequest {
                path: path.to_owned(),
                recursive,
            },
        ))
        .await
    }

    /// Renames or moves a path.
    pub async fn storage_rename(
        &self,
        old_path: &str,
        new_path: &str,
    ) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::StorageRenameRequest(
            pb_storage::RenameRequest {
                old_path: old_path.to_owned(),
                new_path: new_path.to_owned(),
            },
        ))
        .await
    }

    /// Returns the MD5 checksum computed by the device.
    pub async fn storage_md5sum(&self, path: &str) -> Result<String, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::StorageMd5sumRequest(
                pb_storage::Md5sumRequest {
                    path: path.to_owned(),
                },
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::StorageMd5sumResponse(response)) => Ok(response.md5sum),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Reboots the device into the requested mode.
    pub async fn reboot(
        &self,
        mode: pb_system::reboot_request::RebootMode,
    ) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::SystemRebootRequest(
            pb_system::RebootRequest { mode: mode as i32 },
        ))
        .await
    }

    /// Performs a factory reset.
    pub async fn factory_reset(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::SystemFactoryResetRequest(
            pb_system::FactoryResetRequest {},
        ))
        .await
    }

    /// Reads the device clock.
    pub async fn get_datetime(&self) -> Result<pb_system::DateTime, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::SystemGetDatetimeRequest(
                pb_system::GetDateTimeRequest {},
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::SystemGetDatetimeResponse(response)) => {
                response.datetime.ok_or(ProtocolError::UnexpectedResponse)
            }
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Sets the device clock.
    pub async fn set_datetime(&self, datetime: pb_system::DateTime) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::SystemSetDatetimeRequest(
            pb_system::SetDateTimeRequest {
                datetime: Some(datetime),
            },
        ))
        .await
    }

    /// Asks the firmware to install an update from a manifest on the SD card.
    pub async fn system_update(
        &self,
        manifest_path: &str,
    ) -> Result<pb_system::update_response::UpdateResultCode, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::SystemUpdateRequest(
                pb_system::UpdateRequest {
                    update_manifest: manifest_path.to_owned(),
                },
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::SystemUpdateResponse(response)) => Ok(
                pb_system::update_response::UpdateResultCode::try_from(response.code)
                    .unwrap_or(pb_system::update_response::UpdateResultCode::UnspecifiedError),
            ),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Starts streaming screen frames as broadcast messages.
    pub async fn start_screen_stream(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GuiStartScreenStreamRequest(
            pb_gui::StartScreenStreamRequest {},
        ))
        .await
    }

    /// Stops the screen stream.
    pub async fn stop_screen_stream(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GuiStopScreenStreamRequest(
            pb_gui::StopScreenStreamRequest {},
        ))
        .await
    }

    /// Sends a virtual button event to the device.
    pub async fn send_input(
        &self,
        key: pb_gui::InputKey,
        input_type: pb_gui::InputType,
    ) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GuiSendInputEventRequest(
            pb_gui::SendInputEventRequest {
                key: key as i32,
                r#type: input_type as i32,
            },
        ))
        .await
    }

    /// Pushes a frame to the virtual display.
    pub async fn send_screen_frame(&self, data: Vec<u8>) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GuiScreenFrame(pb_gui::ScreenFrame {
            data,
            orientation: pb_gui::ScreenOrientation::Horizontal as i32,
        }))
        .await
    }

    /// Starts a virtual display session.
    pub async fn start_virtual_display(
        &self,
        first_frame: Option<Vec<u8>>,
    ) -> Result<(), ProtocolError> {
        let first_frame = first_frame.map(|data| pb_gui::ScreenFrame {
            data,
            orientation: pb_gui::ScreenOrientation::Horizontal as i32,
        });

        self.expect_empty(pb::main::Content::GuiStartVirtualDisplayRequest(
            pb_gui::StartVirtualDisplayRequest {
                first_frame,
                send_input: true,
            },
        ))
        .await
    }

    /// Stops the virtual display session.
    pub async fn stop_virtual_display(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GuiStopVirtualDisplayRequest(
            pb_gui::StopVirtualDisplayRequest {},
        ))
        .await
    }

    /// Reads the device-side modification time of a path.
    pub async fn storage_timestamp(&self, path: &str) -> Result<u32, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::StorageTimestampRequest(
                pb_storage::TimestampRequest {
                    path: path.to_owned(),
                },
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::StorageTimestampResponse(response)) => Ok(response.timestamp),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Extracts a tar archive already stored on the device.
    pub async fn storage_tar_extract(
        &self,
        tar_path: &str,
        out_path: &str,
    ) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::StorageTarExtractRequest(
            pb_storage::TarExtractRequest {
                tar_path: tar_path.to_owned(),
                out_path: out_path.to_owned(),
            },
        ))
        .await
    }

    /// Reads battery and power information.
    pub async fn system_power_info(&self) -> Result<Vec<(String, String)>, ProtocolError> {
        let responses = self
            .call(pb::main::Content::SystemPowerInfoRequest(
                pb_system::PowerInfoRequest {},
            ))
            .await?;

        let mut properties = Vec::new();

        for response in responses {
            match response.content {
                Some(pb::main::Content::SystemPowerInfoResponse(response)) => {
                    properties.push((response.key, response.value));
                }
                Some(pb::main::Content::Empty(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(properties)
    }

    /// Plays the device's visual/audible alert.
    pub async fn system_play_audiovisual_alert(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::SystemPlayAudiovisualAlertRequest(
            pb_system::PlayAudiovisualAlertRequest {},
        ))
        .await
    }

    /// Returns whether the desktop is locked.
    ///
    /// The firmware encodes the answer in `command_status`: `OK` means locked
    /// and `ERROR` means unlocked, so the generic status check is bypassed.
    pub async fn desktop_is_locked(&self) -> Result<bool, ProtocolError> {
        match self
            .call(pb::main::Content::DesktopIsLockedRequest(
                pb_desktop::IsLockedRequest {},
            ))
            .await
        {
            Ok(_) => Ok(true),
            Err(ProtocolError::Device {
                status: pb::CommandStatus::Error,
                ..
            }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Unlocks the desktop.
    pub async fn desktop_unlock(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::DesktopUnlockRequest(
            pb_desktop::UnlockRequest {},
        ))
        .await
    }

    /// Subscribes to desktop lock state broadcasts.
    pub async fn desktop_status_subscribe(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::DesktopStatusSubscribeRequest(
            pb_desktop::StatusSubscribeRequest {},
        ))
        .await
    }

    /// Unsubscribes from desktop lock state broadcasts.
    pub async fn desktop_status_unsubscribe(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::DesktopStatusUnsubscribeRequest(
            pb_desktop::StatusUnsubscribeRequest {},
        ))
        .await
    }

    /// Starts an application by name.
    pub async fn app_start(&self, name: &str, args: &str) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::AppStartRequest(pb_app::StartRequest {
            name: name.to_owned(),
            args: args.to_owned(),
        }))
        .await
    }

    /// Asks the running application to exit.
    pub async fn app_exit(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::AppExitRequest(pb_app::AppExitRequest {}))
            .await
    }

    /// Loads a file in the running application.
    pub async fn app_load_file(&self, path: &str) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::AppLoadFileRequest(
            pb_app::AppLoadFileRequest {
                path: path.to_owned(),
            },
        ))
        .await
    }

    /// Sends a button press event to the running application.
    pub async fn app_button_press(&self, args: &str, index: i32) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::AppButtonPressRequest(
            pb_app::AppButtonPressRequest {
                args: args.to_owned(),
                index,
            },
        ))
        .await
    }

    /// Sends a button release event to the running application.
    pub async fn app_button_release(&self) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::AppButtonReleaseRequest(
            pb_app::AppButtonReleaseRequest {},
        ))
        .await
    }

    /// Returns whether an application currently holds the loader lock.
    pub async fn app_lock_status(&self) -> Result<bool, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::AppLockStatusRequest(
                pb_app::LockStatusRequest {},
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::AppLockStatusResponse(response)) => Ok(response.locked),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Returns the last application error.
    pub async fn app_get_error(&self) -> Result<(u32, String), ProtocolError> {
        let response = self
            .call_one(pb::main::Content::AppGetErrorRequest(
                pb_app::GetErrorRequest {},
            ))
            .await?;

        match response.content {
            Some(pb::main::Content::AppGetErrorResponse(response)) => {
                Ok((response.code, response.text))
            }
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Returns the current mode of a GPIO pin.
    pub async fn gpio_get_mode(&self, pin: i32) -> Result<i32, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::GpioGetPinMode(pb_gpio::GetPinMode {
                pin,
            }))
            .await?;

        match response.content {
            Some(pb::main::Content::GpioGetPinModeResponse(response)) => Ok(response.mode),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Reads a GPIO pin (the pin must be configured as an input).
    pub async fn gpio_read(&self, pin: i32) -> Result<u32, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::GpioReadPin(pb_gpio::ReadPin { pin }))
            .await?;

        match response.content {
            Some(pb::main::Content::GpioReadPinResponse(response)) => Ok(response.value),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Configures a GPIO pin as input or output.
    pub async fn gpio_set_mode(&self, pin: i32, mode: i32) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GpioSetPinMode(pb_gpio::SetPinMode {
            pin,
            mode,
        }))
        .await
    }

    /// Selects the pull resistor of an input pin.
    pub async fn gpio_set_input_pull(&self, pin: i32, pull: i32) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GpioSetInputPull(pb_gpio::SetInputPull {
            pin,
            pull_mode: pull,
        }))
        .await
    }

    /// Drives an output pin.
    pub async fn gpio_write(&self, pin: i32, value: u32) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GpioWritePin(pb_gpio::WritePin {
            pin,
            value,
        }))
        .await
    }

    /// Returns the 5V OTG output state.
    pub async fn gpio_get_otg_mode(&self) -> Result<i32, ProtocolError> {
        let response = self
            .call_one(pb::main::Content::GpioGetOtgMode(pb_gpio::GetOtgMode {}))
            .await?;

        match response.content {
            Some(pb::main::Content::GpioGetOtgModeResponse(response)) => Ok(response.mode),
            _ => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Enables or disables the 5V OTG output.
    pub async fn gpio_set_otg_mode(&self, mode: i32) -> Result<(), ProtocolError> {
        self.expect_empty(pb::main::Content::GpioSetOtgMode(pb_gpio::SetOtgMode {
            mode,
        }))
        .await
    }

    async fn expect_empty(&self, content: pb::main::Content) -> Result<(), ProtocolError> {
        let responses = self.call(content).await?;

        for response in responses {
            match response.content {
                Some(pb::main::Content::Empty(_) | pb::main::Content::SystemPingResponse(_)) => {}
                _ => return Err(ProtocolError::UnexpectedResponse),
            }
        }

        Ok(())
    }
}

fn entry_from_file(file: pb_storage::File) -> StorageEntry {
    let kind = if file.r#type == pb_storage::file::FileType::Dir as i32 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };

    StorageEntry {
        name: file.name,
        kind,
        size_bytes: u64::from(file.size),
    }
}

fn frame(id: u32, outgoing: &Outgoing) -> pb::Main {
    pb::Main {
        command_id: id,
        command_status: 0,
        has_next: outgoing.has_next,
        content: Some(outgoing.content.clone()),
    }
}

async fn run<S>(
    stream: S,
    mut commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<SessionEvent>,
) where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (mut sink, mut incoming) = Framed::new(stream, FrameCodec).split();

    let (writer, mut writer_rx) = mpsc::channel::<pb::Main>(32);
    let writer_task = tokio::spawn(async move {
        while let Some(message) = writer_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut pending: HashMap<u32, Pending> = HashMap::new();
    let mut next_id: u32 = 1;
    let mut close_reason: Option<String> = None;

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    None | Some(Command::Shutdown) => break,
                    Some(Command::Request { outgoing, reply }) => {
                        let id = allocate_id(&mut next_id);

                        if outgoing.is_empty() {
                            let _ = reply.send(Ok(Vec::new()));
                            continue;
                        }

                        pending.insert(id, Pending {
                            responses: Vec::new(),
                            reply,
                        });

                        let mut writer_failed = false;

                        for outgoing in &outgoing {
                            if writer.send(frame(id, outgoing)).await.is_err() {
                                writer_failed = true;
                                break;
                            }
                        }

                        if writer_failed {
                            if let Some(pending) = pending.remove(&id) {
                                let _ = pending.reply.send(Err(ProtocolError::Closed(
                                    String::from("writer task stopped"),
                                )));
                            }
                            break;
                        }
                    }
                }
            }
            message = incoming.next() => {
                match message {
                    Some(Ok(message)) => {
                        if message.command_id == 0 {
                            let _ = events.send(SessionEvent::Broadcast(message));
                            continue;
                        }

                        if message.command_status != 0 {
                            if let Some(pending) = pending.remove(&message.command_id) {
                                let status = pb::CommandStatus::try_from(message.command_status)
                                    .unwrap_or(pb::CommandStatus::Error);
                                let _ = pending.reply.send(Err(ProtocolError::Device {
                                    status,
                                    message: status_label(message.command_status),
                                }));
                            }
                            continue;
                        }

                        let (id, finished) = {
                            let Some(pending) = pending.get_mut(&message.command_id) else {
                                tracing::trace!(
                                    "dropping response for unknown command {}",
                                    message.command_id
                                );
                                continue;
                            };

                            let id = message.command_id;
                            let has_next = message.has_next;
                            pending.responses.push(message);
                            (id, !has_next)
                        };

                        if finished
                            && let Some(pending) = pending.remove(&id)
                        {
                            let _ = pending.reply.send(Ok(pending.responses));
                        }
                    }
                    Some(Err(error)) => {
                        close_reason = Some(error.to_string());
                        break;
                    }
                    None => {
                        close_reason = Some(String::from("device closed the connection"));
                        break;
                    }
                }
            }
        }
    }

    drop(writer);
    writer_task.abort();

    let reason = close_reason
        .clone()
        .unwrap_or_else(|| String::from("session stopped"));

    for (_, pending) in pending.drain() {
        let _ = pending
            .reply
            .send(Err(ProtocolError::Closed(reason.clone())));
    }

    let _ = events.send(SessionEvent::Closed(close_reason));
}

fn allocate_id(next_id: &mut u32) -> u32 {
    let id = *next_id;
    *next_id = next_id.wrapping_add(1);

    if *next_id == 0 {
        *next_id = 1;
    }

    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{codec::FrameCodec, pb_system};

    /// A tiny scripted device that answers frames on the other end of a
    /// duplex pipe.
    fn spawn_fake_device<S>(stream: S) -> tokio::task::JoinHandle<()>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        tokio::spawn(async move {
            let mut framed = Framed::new(stream, FrameCodec);

            while let Some(Ok(message)) = framed.next().await {
                let id = message.command_id;
                let has_next = message.has_next;

                match message.content {
                    Some(pb::main::Content::SystemProtobufVersionRequest(_)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::SystemProtobufVersionResponse(
                                pb_system::ProtobufVersionResponse {
                                    major: 0,
                                    minor: 21,
                                },
                            )),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::StorageReadRequest(_)) => {
                        for (index, has_next) in [(&b"hello"[..], true), (&b" world"[..], false)] {
                            let response = pb::Main {
                                command_id: id,
                                command_status: 0,
                                has_next,
                                content: Some(pb::main::Content::StorageReadResponse(
                                    pb_storage::ReadResponse {
                                        file: Some(pb_storage::File {
                                            data: index.to_vec(),
                                            ..pb_storage::File::default()
                                        }),
                                    },
                                )),
                            };
                            framed.send(response).await.unwrap();
                        }
                    }
                    Some(pb::main::Content::SystemPowerInfoRequest(_)) => {
                        for (index, (key, value, has_next)) in
                            [("battery_level", "87", true), ("charge_level", "1", false)]
                                .into_iter()
                                .enumerate()
                        {
                            let _ = index;
                            let response = pb::Main {
                                command_id: id,
                                command_status: 0,
                                has_next,
                                content: Some(pb::main::Content::SystemPowerInfoResponse(
                                    pb_system::PowerInfoResponse {
                                        key: key.to_owned(),
                                        value: value.to_owned(),
                                    },
                                )),
                            };
                            framed.send(response).await.unwrap();
                        }
                    }
                    Some(pb::main::Content::DesktopIsLockedRequest(_)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::Empty(pb::Empty {})),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::AppLockStatusRequest(_)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::AppLockStatusResponse(
                                pb_app::LockStatusResponse { locked: true },
                            )),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::GpioReadPin(request)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::GpioReadPinResponse(
                                pb_gpio::ReadPinResponse {
                                    value: u32::from(request.pin == 0),
                                },
                            )),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::StorageTimestampRequest(_)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::StorageTimestampResponse(
                                pb_storage::TimestampResponse {
                                    timestamp: 1_700_000_000,
                                },
                            )),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::StorageStatRequest(_)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::StorageStatResponse(
                                pb_storage::StatResponse {
                                    file: Some(pb_storage::File {
                                        size: 42,
                                        ..pb_storage::File::default()
                                    }),
                                },
                            )),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::StorageWriteRequest(_)) if has_next => {}
                    Some(pb::main::Content::StorageWriteRequest(_)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::Empty(pb::Empty {})),
                        };
                        framed.send(response).await.unwrap();
                    }
                    Some(pb::main::Content::SystemPingRequest(request)) => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: 0,
                            has_next: false,
                            content: Some(pb::main::Content::SystemPingResponse(
                                pb_system::PingResponse { data: request.data },
                            )),
                        };
                        framed.send(response).await.unwrap();
                    }
                    _ => {
                        let response = pb::Main {
                            command_id: id,
                            command_status: pb::CommandStatus::Error as i32,
                            has_next: false,
                            content: Some(pb::main::Content::Empty(pb::Empty {})),
                        };
                        framed.send(response).await.unwrap();
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn reads_the_protobuf_version() {
        let (client, server) = tokio::io::duplex(4096);
        let device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let version = session.protobuf_version().await.unwrap();
        assert_eq!(version, ProtobufVersion::new(0, 21));

        session.stop().await;
        drop(session);
        device.await.unwrap();
    }

    #[tokio::test]
    async fn assembles_multi_part_responses() {
        let (client, server) = tokio::io::duplex(4096);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let data = session.storage_read("/ext/test.txt").await.unwrap();
        assert_eq!(data, b"hello world");
    }

    #[tokio::test]
    async fn sends_every_write_chunk_within_one_exchange() {
        let (client, server) = tokio::io::duplex(8192);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let data = vec![0x5A; STORAGE_CHUNK_SIZE + 10];
        session.storage_write("/ext/blob.bin", &data).await.unwrap();
    }

    #[tokio::test]
    async fn reads_storage_timestamps() {
        let (client, server) = tokio::io::duplex(4096);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let timestamp = session.storage_timestamp("/ext/Manifest").await.unwrap();
        assert_eq!(timestamp, 1_700_000_000);
    }

    #[tokio::test]
    async fn falls_back_to_the_requested_path_for_unnamed_stats() {
        let (client, server) = tokio::io::duplex(4096);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let entry = session
            .storage_stat("/ext/Manifest")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(entry.name, "Manifest");
        assert_eq!(entry.size_bytes, 42);
        assert!(!entry.is_directory());
    }

    #[tokio::test]
    async fn reads_power_and_desktop_state() {
        let (client, server) = tokio::io::duplex(4096);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let power = session.system_power_info().await.unwrap();
        assert_eq!(
            power,
            vec![
                ("battery_level".to_owned(), "87".to_owned()),
                ("charge_level".to_owned(), "1".to_owned()),
            ]
        );

        assert!(session.desktop_is_locked().await.unwrap());
        assert!(session.app_lock_status().await.unwrap());
        assert_eq!(session.gpio_read(0).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn echoes_ping_payloads() {
        let (client, server) = tokio::io::duplex(4096);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let echoed = session.ping(vec![1, 2, 3, 4]).await.unwrap();
        assert_eq!(echoed, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn reports_device_errors() {
        let (client, server) = tokio::io::duplex(4096);
        let _device = spawn_fake_device(server);
        let session = RpcSession::spawn(client);

        let error = session
            .storage_list("/ext")
            .await
            .expect_err("fake device errors on list");

        assert!(matches!(error, ProtocolError::Device { .. }));
    }
}
