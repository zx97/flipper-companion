//! Flipper screen frames (128×64, 1 bit per pixel).
//!
//! The firmware sends raw framebuffer bytes, row-major, most-significant bit
//! first.  This happens to be exactly the packing used by the binary PBM
//! format, so exporting a frame is a header concatenation.

use crate::{error::ProtocolError, pb, pb_gui};

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
pub const ROW_BYTES: usize = WIDTH / 8;
pub const FRAME_BYTES: usize = ROW_BYTES * HEIGHT;

/// An owned, validated screen frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFrame {
    pub data: Vec<u8>,
    pub orientation: pb_gui::ScreenOrientation,
}

impl ScreenFrame {
    /// Extracts a frame from a broadcast `PB_Main` message.
    #[must_use]
    pub fn from_message(message: &pb::Main) -> Option<Self> {
        let pb::main::Content::GuiScreenFrame(frame) = message.content.as_ref()? else {
            return None;
        };

        let orientation = pb_gui::ScreenOrientation::try_from(frame.orientation)
            .unwrap_or(pb_gui::ScreenOrientation::Horizontal);

        Some(Self {
            data: frame.data.clone(),
            orientation,
        })
    }

    /// Validates that the frame has the expected size.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.data.len() < FRAME_BYTES {
            return Err(ProtocolError::UnexpectedResponse);
        }

        Ok(())
    }

    /// Exports the frame as a binary PBM (`P4`) image.
    ///
    /// PBM is row-major, so the page-major device buffer is converted back to
    /// rows of 16 bytes, most-significant bit first.
    pub fn to_pbm(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;

        let mut pbm = format!("P4\n{WIDTH} {HEIGHT}\n").into_bytes();

        for y in 0..HEIGHT {
            let mut row = [0u8; ROW_BYTES];

            for x in 0..WIDTH {
                if self.pixel(x, y) {
                    row[x / 8] |= 1 << (7 - (x % 8));
                }
            }

            pbm.extend_from_slice(&row);
        }

        Ok(pbm)
    }

    /// Returns one pixel of the page-major SSD1306 framebuffer.
    ///
    /// The firmware transmits the display buffer as eight vertical pages:
    /// byte index `(y / 8) * WIDTH + x`, bit `y % 8`.
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= WIDTH || y >= HEIGHT {
            return false;
        }

        let index = (y / 8) * WIDTH + x;

        self.data
            .get(index)
            .is_some_and(|byte| byte & (1 << (y % 8)) != 0)
    }

    /// Converts the frame to RGBA8, black pixels on white.
    ///
    /// The result is `WIDTH * HEIGHT * 4` bytes, suitable for
    /// `slint::Image::from_rgba8`.
    pub fn to_rgba(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;

        let mut rgba = Vec::with_capacity(WIDTH * HEIGHT * 4);

        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if self.pixel(x, y) {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                } else {
                    rgba.extend_from_slice(&[255, 255, 255, 255]);
                }
            }
        }

        Ok(rgba)
    }

    /// Renders the frame as 128×64 ASCII art, for terminals and logs.
    #[must_use]
    pub fn to_ascii(&self) -> String {
        let mut output = String::with_capacity((WIDTH + 1) * HEIGHT);

        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                output.push(if self.pixel(x, y) { '#' } else { ' ' });
            }

            output.push('\n');
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(byte: u8) -> ScreenFrame {
        ScreenFrame {
            data: vec![byte; FRAME_BYTES],
            orientation: pb_gui::ScreenOrientation::Horizontal,
        }
    }

    #[test]
    fn writes_a_valid_pbm_header() {
        let pbm = frame_with(0).to_pbm().unwrap();
        assert!(pbm.starts_with(b"P4\n128 64\n"));
        assert_eq!(pbm.len(), b"P4\n128 64\n".len() + FRAME_BYTES);
    }

    #[test]
    fn renders_ascii_rows() {
        let mut frame = frame_with(0);
        frame.data[0] = 0x01; // pixel (0, 0)

        let ascii = frame.to_ascii();
        let first = ascii.lines().next().unwrap();

        assert_eq!(first.len(), WIDTH);
        assert!(first.starts_with('#'));
        assert!(first.ends_with(' '));
    }

    #[test]
    fn decodes_page_major_bits() {
        let mut frame = frame_with(0);
        frame.data[0] = 0b0000_0001; // pixel (0, 0)
        frame.data[1] = 0b0000_0001; // pixel (1, 0)

        let rgba = frame.to_rgba().unwrap();
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 0, 0, 255]);
        assert_eq!(&rgba[8..12], &[255, 255, 255, 255]);

        let pbm = frame.to_pbm().unwrap();
        let header = b"P4\n128 64\n".len();
        assert_eq!(pbm[header], 0b1100_0000);
        assert_eq!(pbm[header + 1], 0);

        assert!(frame.to_ascii().starts_with("##"));
    }

    #[test]
    fn converts_to_rgba() {
        let rgba = frame_with(0xFF).to_rgba().unwrap();

        assert_eq!(rgba.len(), WIDTH * HEIGHT * 4);
        assert_eq!(&rgba[..4], &[0, 0, 0, 255]);

        let rgba = frame_with(0x00).to_rgba().unwrap();
        assert_eq!(&rgba[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn rejects_short_frames() {
        let frame = ScreenFrame {
            data: vec![0; 10],
            orientation: pb_gui::ScreenOrientation::Horizontal,
        };

        assert!(frame.to_pbm().is_err());
    }
}
