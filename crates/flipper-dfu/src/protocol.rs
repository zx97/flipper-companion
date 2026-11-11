//! The STM32 DFU protocol, expressed as a small, explicit state machine.
//!
//! The state machine is generic over [`Transport`], which makes the whole
//! flashing sequence testable without a physical device.

use std::{
    thread,
    time::{Duration, Instant},
};

use crate::{
    dfuse::{DfuseFile, Target},
    error::DfuError,
    layout::MemoryLayout,
};

pub const DFU_DETACH: u8 = 0;
pub const DFU_DNLOAD: u8 = 1;
pub const DFU_UPLOAD: u8 = 2;
pub const DFU_GETSTATUS: u8 = 3;
pub const DFU_CLRSTATUS: u8 = 4;
pub const DFU_GETSTATE: u8 = 5;
pub const DFU_ABORT: u8 = 6;

const STATUS_LENGTH: usize = 6;
const DFU_FUNCTIONAL_DESCRIPTOR: u8 = 0x21;
const DFU_FUNCTIONAL_DESCRIPTOR_LENGTH: usize = 9;
const MAX_TRANSFER_SIZE: usize = 4096;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Low-level operations needed to drive a DFU interface.
///
/// Implementations only perform USB control transfers; all sequencing lives
/// in [`DfuDevice`].
pub trait Transport {
    /// Selects an alternate setting on the DFU interface.
    fn set_alternate_setting(&mut self, alternate: u8) -> Result<(), DfuError>;

    /// Class/interface control-IN request.
    fn control_in(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
    ) -> Result<Vec<u8>, DfuError>;

    /// Class/interface control-OUT request.
    fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), DfuError>;

    /// Returns the memory layout string for an alternate setting.
    fn interface_string(&mut self, alternate: u8) -> Result<String, DfuError>;

    /// Returns the DFU functional descriptor (type `0x21`) of the interface.
    fn functional_descriptor(&mut self, interface: u8) -> Result<Vec<u8>, DfuError>;
}

/// DFU device state, as reported by `GETSTATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfuState {
    AppIdle,
    AppDetach,
    DfuIdle,
    DnLoadSync,
    DnBusy,
    DnLoadIdle,
    ManifestSync,
    Manifest,
    ManifestWaitReset,
    UploadIdle,
    Error,
    Unknown(u8),
}

impl From<u8> for DfuState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::AppIdle,
            1 => Self::AppDetach,
            2 => Self::DfuIdle,
            3 => Self::DnLoadSync,
            4 => Self::DnBusy,
            5 => Self::DnLoadIdle,
            6 => Self::ManifestSync,
            7 => Self::Manifest,
            8 => Self::ManifestWaitReset,
            9 => Self::UploadIdle,
            10 => Self::Error,
            other => Self::Unknown(other),
        }
    }
}

/// DFU status code, as reported by `GETSTATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    ErrTarget,
    ErrFile,
    ErrWrite,
    ErrErase,
    ErrCheckErased,
    ErrProgram,
    ErrVerify,
    ErrAddress,
    ErrNotDone,
    ErrFirmware,
    ErrVendor,
    ErrUsbReset,
    ErrPowerOnReset,
    ErrUnknown,
    ErrStalledPacket,
    Unknown(u8),
}

impl From<u8> for StatusCode {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Ok,
            1 => Self::ErrTarget,
            2 => Self::ErrFile,
            3 => Self::ErrWrite,
            4 => Self::ErrErase,
            5 => Self::ErrCheckErased,
            6 => Self::ErrProgram,
            7 => Self::ErrVerify,
            8 => Self::ErrAddress,
            9 => Self::ErrNotDone,
            10 => Self::ErrFirmware,
            11 => Self::ErrVendor,
            12 => Self::ErrUsbReset,
            13 => Self::ErrPowerOnReset,
            14 => Self::ErrUnknown,
            15 => Self::ErrStalledPacket,
            other => Self::Unknown(other),
        }
    }
}

impl StatusCode {
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Decoded `GETSTATUS` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub code: StatusCode,
    pub poll_timeout: Duration,
    pub state: DfuState,
    pub string_index: u8,
}

impl Status {
    fn parse(bytes: &[u8]) -> Result<Self, DfuError> {
        if bytes.len() != STATUS_LENGTH {
            return Err(DfuError::Protocol(format!(
                "expected a {STATUS_LENGTH}-byte status, got {}",
                bytes.len()
            )));
        }

        let poll_ms =
            u32::from(bytes[1]) | (u32::from(bytes[2]) << 8) | (u32::from(bytes[3]) << 16);

        Ok(Self {
            code: bytes[0].into(),
            poll_timeout: Duration::from_millis(u64::from(poll_ms)),
            state: bytes[4].into(),
            string_index: bytes[5],
        })
    }
}

/// Which phase of a flash operation progress refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashStage {
    Erasing,
    Writing,
}

/// Progress emitted while flashing an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashProgress {
    pub stage: FlashStage,
    pub done: u64,
    pub total: u64,
}

impl FlashProgress {
    #[must_use]
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

/// Drives a device through the DfuSe download sequence.
pub struct DfuDevice<'a, T: Transport> {
    transport: &'a mut T,
}

impl<'a, T: Transport> DfuDevice<'a, T> {
    #[must_use]
    pub fn new(transport: &'a mut T) -> Self {
        Self { transport }
    }

    /// Erases and writes every element of `image`, reporting progress.
    ///
    /// Progress is reported twice: once for the erase phase (in pages) and
    /// once for the write phase (in bytes).
    pub fn flash(
        &mut self,
        image: &DfuseFile,
        mut on_progress: impl FnMut(FlashProgress),
    ) -> Result<(), DfuError> {
        let plans = self.plan(image)?;

        let total_pages: u64 = plans
            .iter()
            .flat_map(|plan| &plan.page_ranges)
            .map(|pages| pages.len() as u64)
            .sum();
        let mut erased = 0u64;

        for plan in &plans {
            self.transport
                .set_alternate_setting(plan.target.alternate_setting)?;
            self.prepare()?;

            for pages in &plan.page_ranges {
                for &address in pages {
                    self.erase_page(address)?;
                    erased += 1;
                    on_progress(FlashProgress {
                        stage: FlashStage::Erasing,
                        done: erased,
                        total: total_pages,
                    });
                }
            }
        }

        let total_bytes = image.payload_size();
        let mut written = 0u64;

        for plan in &plans {
            self.transport
                .set_alternate_setting(plan.target.alternate_setting)?;
            let transfer_size = self.transfer_size()?;

            for element in &plan.target.elements {
                // Each element restarts from a clean, idle DFU state.
                self.prepare()?;
                self.set_address(element.address)?;

                for (index, chunk) in element.data.chunks(transfer_size).enumerate() {
                    let block = u16::try_from(index.saturating_add(2)).map_err(|_| {
                        DfuError::Protocol(String::from("element is too large to flash"))
                    })?;

                    self.transport.control_out(DFU_DNLOAD, block, 0, chunk)?;
                    self.wait_for_state(DfuState::DnLoadIdle, "element download")?;

                    written += chunk.len() as u64;
                    on_progress(FlashProgress {
                        stage: FlashStage::Writing,
                        done: written,
                        total: total_bytes,
                    });
                }
            }
        }

        Ok(())
    }

    /// Returns the memory layout advertised by an alternate setting.
    pub fn memory_layout(&mut self, alternate: u8) -> Result<MemoryLayout, DfuError> {
        self.transport.set_alternate_setting(alternate)?;

        let descriptor = self.transport.interface_string(alternate)?;
        MemoryLayout::parse(&descriptor)
    }

    /// Uploads a block of device memory.
    ///
    /// Used by the STM32WB55 recovery operations to read OTP, option bytes,
    /// the FUS state and the radio version tables.
    pub fn upload(
        &mut self,
        address: u32,
        size: usize,
        alternate: u8,
    ) -> Result<Vec<u8>, DfuError> {
        if size == 0 {
            return Ok(Vec::new());
        }

        self.transport.set_alternate_setting(alternate)?;
        self.prepare()?;
        self.abort()?;
        self.set_address(address)?;
        self.abort()?;

        let transfer_size = self.transfer_size()?;
        let mut data = Vec::with_capacity(size);
        let mut block = 2u16;

        while data.len() < size {
            let remaining = size - data.len();
            let chunk_size = remaining.min(transfer_size);
            let chunk = self
                .transport
                .control_in(DFU_UPLOAD, block, 0, chunk_size)?;

            if chunk.is_empty() {
                break;
            }

            let short = chunk.len() < chunk_size;
            data.extend_from_slice(&chunk);

            if short {
                break;
            }

            block = block.wrapping_add(1);
        }

        data.truncate(size);
        Ok(data)
    }

    /// Erases every page overlapping `[address, address + size)`.
    pub fn erase_range(&mut self, address: u32, size: usize) -> Result<(), DfuError> {
        self.transport.set_alternate_setting(0)?;

        let descriptor = self.transport.interface_string(0)?;
        let layout = MemoryLayout::parse(&descriptor)?;
        let length = u32::try_from(size)
            .map_err(|_| DfuError::Protocol(String::from("erase size exceeds u32")))?;
        let end = address
            .checked_add(length)
            .ok_or(DfuError::ElementOutsideLayout { address })?;

        if address < layout.base_address || end > layout.end_address() {
            return Err(DfuError::ElementOutsideLayout { address });
        }

        let pages = layout.page_addresses(address, end);
        if pages.is_empty() {
            return Err(DfuError::ElementOutsideLayout { address });
        }

        self.prepare()?;

        for page in pages {
            self.erase_page(page)?;
        }

        Ok(())
    }

    /// Writes `data` at `address` without erasing first.
    pub fn download_bytes(
        &mut self,
        address: u32,
        data: &[u8],
        alternate: u8,
    ) -> Result<(), DfuError> {
        self.transport.set_alternate_setting(alternate)?;
        self.prepare()?;
        self.set_address(address)?;

        let transfer_size = self.transfer_size()?;

        for (index, chunk) in data.chunks(transfer_size).enumerate() {
            let block = u16::try_from(index.saturating_add(2))
                .map_err(|_| DfuError::Protocol(String::from("buffer is too large")))?;

            self.transport.control_out(DFU_DNLOAD, block, 0, chunk)?;
            self.wait_for_state(DfuState::DnLoadIdle, "element download")?;
        }

        Ok(())
    }

    /// Sends a raw `DNLOAD` block 0 command (used by STM32WB55 FUS commands).
    pub fn download_raw(&mut self, data: &[u8]) -> Result<(), DfuError> {
        self.transport.set_alternate_setting(0)?;
        self.prepare()?;
        self.transport.control_out(DFU_DNLOAD, 0, 0, data)?;
        self.wait_for_state(DfuState::DnLoadIdle, "raw download")
    }

    /// Puts the device in the idle state, recovering from errors if needed.
    pub fn prepare(&mut self) -> Result<(), DfuError> {
        for _ in 0..2 {
            let status = self.status()?;

            if status.code.is_ok() && status.state == DfuState::DfuIdle {
                return Ok(());
            }

            if !status.code.is_ok() {
                tracing::debug!("clearing stale DFU error state {:?}", status.code);
                self.clear_status()?;
            } else {
                tracing::debug!("aborting DFU state {:?}", status.state);
                self.abort()?;
            }
        }

        Err(DfuError::Protocol(String::from(
            "device did not return to the DFU idle state",
        )))
    }

    /// Issues `DFU_ABORT` and verifies that the device is idle.
    pub fn abort(&mut self) -> Result<(), DfuError> {
        self.transport.control_out(DFU_ABORT, 0, 0, &[])?;
        let status = self.status()?;

        if status.code.is_ok() && status.state == DfuState::DfuIdle {
            Ok(())
        } else {
            Err(DfuError::Device {
                status: status_byte(status.code),
                state: state_byte(status.state),
            })
        }
    }

    /// Gracefully leaves DFU mode (used after wireless-stack updates).
    pub fn leave(&mut self) -> Result<(), DfuError> {
        self.prepare()?;
        self.abort()?;
        self.set_address(0x080F_FFFF)?;
        self.transport.control_out(DFU_DNLOAD, 0, 0, &[])?;
        let _ = self.status();
        Ok(())
    }

    fn plan<'i>(&mut self, image: &'i DfuseFile) -> Result<Vec<Plan<'i>>, DfuError> {
        let mut plans = Vec::with_capacity(image.targets.len());

        for target in &image.targets {
            self.transport
                .set_alternate_setting(target.alternate_setting)?;

            let descriptor = self.transport.interface_string(target.alternate_setting)?;
            let layout = MemoryLayout::parse(&descriptor)?;

            let mut page_ranges = Vec::with_capacity(target.elements.len());

            for element in &target.elements {
                let length = u32::try_from(element.data.len()).map_err(|_| {
                    DfuError::Protocol(String::from("element exceeds the 32-bit address space"))
                })?;
                let end =
                    element
                        .address
                        .checked_add(length)
                        .ok_or(DfuError::ElementOutsideLayout {
                            address: element.address,
                        })?;

                if element.data.is_empty()
                    || element.address < layout.base_address
                    || end > layout.end_address()
                {
                    return Err(DfuError::ElementOutsideLayout {
                        address: element.address,
                    });
                }

                page_ranges.push(layout.page_addresses(element.address, end));
            }

            plans.push(Plan {
                target,
                page_ranges,
            });
        }

        Ok(plans)
    }

    fn transfer_size(&mut self) -> Result<usize, DfuError> {
        let descriptor = self.transport.functional_descriptor(0)?;

        if descriptor.len() < DFU_FUNCTIONAL_DESCRIPTOR_LENGTH
            || descriptor[1] != DFU_FUNCTIONAL_DESCRIPTOR
        {
            return Err(DfuError::Protocol(String::from(
                "device did not expose a DFU functional descriptor",
            )));
        }

        let size = u16::from_le_bytes([descriptor[5], descriptor[6]]) as usize;

        if size == 0 {
            return Err(DfuError::Protocol(String::from(
                "device reported a zero transfer size",
            )));
        }

        Ok(size.min(MAX_TRANSFER_SIZE))
    }

    fn status(&mut self) -> Result<Status, DfuError> {
        let bytes = self
            .transport
            .control_in(DFU_GETSTATUS, 0, 0, STATUS_LENGTH)?;
        Status::parse(&bytes)
    }

    fn clear_status(&mut self) -> Result<(), DfuError> {
        self.transport.control_out(DFU_CLRSTATUS, 0, 0, &[])
    }

    fn set_address(&mut self, address: u32) -> Result<(), DfuError> {
        let mut request = [0u8; 5];
        request[0] = 0x21;
        request[1..].copy_from_slice(&address.to_le_bytes());

        self.transport.control_out(DFU_DNLOAD, 0, 0, &request)?;
        self.wait_for_state(DfuState::DnLoadIdle, "address pointer")
    }

    fn erase_page(&mut self, address: u32) -> Result<(), DfuError> {
        let mut request = [0u8; 5];
        request[0] = 0x41;
        request[1..].copy_from_slice(&address.to_le_bytes());

        self.transport.control_out(DFU_DNLOAD, 0, 0, &request)?;
        self.wait_for_state(DfuState::DnLoadIdle, "page erase")
    }

    fn wait_for_state(&mut self, expected: DfuState, what: &'static str) -> Result<(), DfuError> {
        let deadline = Instant::now() + OPERATION_TIMEOUT;

        loop {
            let status = self.status()?;

            if !status.code.is_ok() {
                return Err(DfuError::Device {
                    status: status_byte(status.code),
                    state: state_byte(status.state),
                });
            }

            if status.state == expected {
                return Ok(());
            }

            if status.state == DfuState::Error {
                return Err(DfuError::Protocol(format!(
                    "device entered the error state while waiting for {what}"
                )));
            }

            if Instant::now() >= deadline {
                return Err(DfuError::Timeout(what));
            }

            if !status.poll_timeout.is_zero() {
                thread::sleep(status.poll_timeout);
            }
        }
    }
}

struct Plan<'i> {
    target: &'i Target,
    page_ranges: Vec<Vec<u32>>,
}

fn status_byte(code: StatusCode) -> u8 {
    match code {
        StatusCode::Ok => 0,
        StatusCode::ErrTarget => 1,
        StatusCode::ErrFile => 2,
        StatusCode::ErrWrite => 3,
        StatusCode::ErrErase => 4,
        StatusCode::ErrCheckErased => 5,
        StatusCode::ErrProgram => 6,
        StatusCode::ErrVerify => 7,
        StatusCode::ErrAddress => 8,
        StatusCode::ErrNotDone => 9,
        StatusCode::ErrFirmware => 10,
        StatusCode::ErrVendor => 11,
        StatusCode::ErrUsbReset => 12,
        StatusCode::ErrPowerOnReset => 13,
        StatusCode::ErrUnknown => 14,
        StatusCode::ErrStalledPacket => 15,
        StatusCode::Unknown(value) => value,
    }
}

fn state_byte(state: DfuState) -> u8 {
    match state {
        DfuState::AppIdle => 0,
        DfuState::AppDetach => 1,
        DfuState::DfuIdle => 2,
        DfuState::DnLoadSync => 3,
        DfuState::DnBusy => 4,
        DfuState::DnLoadIdle => 5,
        DfuState::ManifestSync => 6,
        DfuState::Manifest => 7,
        DfuState::ManifestWaitReset => 8,
        DfuState::UploadIdle => 9,
        DfuState::Error => 10,
        DfuState::Unknown(value) => value,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::{
        dfuse::{DfuseFile, Element, Suffix, Target},
        layout::PageBank,
    };

    /// A scripted transport: replies to GETSTATUS with a queue of statuses
    /// and records every control transfer.
    struct MockTransport {
        statuses: VecDeque<[u8; STATUS_LENGTH]>,
        transfers: Vec<(u8, u16, Vec<u8>)>,
        layout: String,
        transfer_size: u16,
        upload: Vec<u8>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                statuses: VecDeque::new(),
                transfers: Vec::new(),
                layout: String::from("flash/0x08000000/2*1Kg"),
                transfer_size: 4,
                upload: Vec::new(),
            }
        }

        fn status_ok(state: u8) -> [u8; STATUS_LENGTH] {
            [0, 0, 0, 0, state, 0]
        }
    }

    impl Transport for MockTransport {
        fn set_alternate_setting(&mut self, _alternate: u8) -> Result<(), DfuError> {
            Ok(())
        }

        fn control_in(
            &mut self,
            request: u8,
            _value: u16,
            _index: u16,
            length: usize,
        ) -> Result<Vec<u8>, DfuError> {
            if request == DFU_GETSTATUS {
                let status = self.statuses.pop_front().expect("unexpected GETSTATUS");
                assert_eq!(length, STATUS_LENGTH);
                return Ok(status.to_vec());
            }

            assert_eq!(request, DFU_UPLOAD);
            let taken = length.min(self.upload.len());
            Ok(self.upload.drain(..taken).collect())
        }

        fn control_out(
            &mut self,
            request: u8,
            value: u16,
            _index: u16,
            data: &[u8],
        ) -> Result<(), DfuError> {
            self.transfers.push((request, value, data.to_vec()));
            Ok(())
        }

        fn interface_string(&mut self, _alternate: u8) -> Result<String, DfuError> {
            Ok(self.layout.clone())
        }

        fn functional_descriptor(&mut self, _interface: u8) -> Result<Vec<u8>, DfuError> {
            let mut descriptor = vec![0u8; 9];
            descriptor[1] = 0x21;
            descriptor[5..7].copy_from_slice(&self.transfer_size.to_le_bytes());
            Ok(descriptor)
        }
    }

    fn image(payload: Vec<u8>) -> DfuseFile {
        DfuseFile {
            version: 1,
            image_size: 0,
            targets: vec![Target {
                alternate_setting: 0,
                name: String::from("flash"),
                declared_size: payload.len() as u32,
                elements: vec![Element {
                    address: 0x0800_0000,
                    data: payload,
                }],
            }],
            suffix: Suffix {
                device_version: 0,
                product_id: 0,
                vendor_id: 0,
                dfu_version: 0,
                crc: 0,
            },
            crc: 0,
        }
    }

    #[test]
    fn flashes_every_page_then_every_chunk() {
        let image = image(vec![0xAA; 8]);
        let mut transport = MockTransport::new();
        transport.layout = String::from("flash/0x08000000/2*1Kg");

        // prepare(idle), erase(busy, idle), prepare(idle), set-address(busy,
        // idle), chunk 0(busy, idle), chunk 1(busy, idle).
        for state in [2, 4, 5, 2, 4, 5, 4, 5, 4, 5] {
            transport
                .statuses
                .push_back(MockTransport::status_ok(state));
        }

        let mut progress = Vec::new();
        DfuDevice::new(&mut transport)
            .flash(&image, |event| progress.push(event))
            .unwrap();

        let dnloads: Vec<_> = transport
            .transfers
            .iter()
            .filter(|(request, _, _)| *request == DFU_DNLOAD)
            .collect();

        // One erase, one set-address, two chunks.
        assert_eq!(dnloads.len(), 4);
        assert_eq!(dnloads[0].2[0], 0x41);
        assert_eq!(dnloads[1].2[0], 0x21);
        assert_eq!(dnloads[2].1, 2);
        assert_eq!(dnloads[2].2, vec![0xAA; 4]);
        assert_eq!(dnloads[3].1, 3);
        assert_eq!(dnloads[3].2, vec![0xAA; 4]);

        assert_eq!(progress.len(), 3);
        assert_eq!(progress[0].stage, FlashStage::Erasing);
        assert_eq!(progress[1].stage, FlashStage::Writing);
        assert_eq!(progress.last().unwrap().done, 8);
    }

    #[test]
    fn uploads_device_memory() {
        let mut transport = MockTransport::new();
        transport.upload = vec![1, 2, 3, 4, 5, 6, 7, 8];

        // prepare(idle), abort(idle), set-address(busy, idle), abort(idle).
        for state in [2, 2, 4, 5, 2] {
            transport
                .statuses
                .push_back(MockTransport::status_ok(state));
        }

        let data = DfuDevice::new(&mut transport)
            .upload(0x2003_0000, 8, 0)
            .unwrap();

        assert_eq!(data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn refuses_elements_outside_the_layout() {
        let mut image = image(vec![0; 16]);
        image.targets[0].elements[0].address = 0x2000_0000;

        let mut transport = MockTransport::new();
        transport.statuses.push_back(MockTransport::status_ok(2));

        let error = DfuDevice::new(&mut transport)
            .flash(&image, |_| {})
            .unwrap_err();

        assert!(matches!(error, DfuError::ElementOutsideLayout { .. }));
    }

    #[test]
    fn refuses_elements_crossing_the_end_of_the_layout() {
        // The layout advertises 2 KiB, the element starts at the base and is
        // one byte too long.
        let mut image = image(vec![0; 2049]);
        image.targets[0].elements[0].address = 0x0800_0000;

        let mut transport = MockTransport::new();
        transport.layout = String::from("flash/0x08000000/2*1Kg");

        let error = DfuDevice::new(&mut transport)
            .flash(&image, |_| {})
            .unwrap_err();

        assert!(matches!(error, DfuError::ElementOutsideLayout { .. }));
    }

    #[test]
    fn rejects_invalid_status_lengths() {
        assert!(Status::parse(&[0; 5]).is_err());
        assert!(Status::parse(&[0; 6]).is_ok());
    }

    #[test]
    fn parses_memory_layout_with_page_bank() {
        let layout = MemoryLayout {
            name: String::from("flash"),
            base_address: 0x0800_0000,
            banks: vec![PageBank {
                pages: 2,
                page_size: 1024,
            }],
        };

        assert_eq!(
            layout.page_addresses(0x0800_0000, 0x0800_0800),
            vec![0x0800_0000, 0x0800_0400]
        );
    }
}
