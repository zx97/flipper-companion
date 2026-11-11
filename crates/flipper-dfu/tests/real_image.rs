//! Optional check against a real, official firmware image.
//!
//! Set `FLIPPER_TEST_DFU=/path/to/full_firmware.dfu` to run it.  The test is
//! skipped otherwise, so the repository does not need to ship a 500 KiB
//! binary fixture.

use flipper_companion_dfu::DfuseFile;

#[test]
fn parses_an_official_image_when_provided() {
    let Some(path) = std::env::var_os("FLIPPER_TEST_DFU") else {
        return;
    };

    let image = DfuseFile::from_path(path).expect("official image must parse");

    assert!(!image.targets.is_empty());
    assert!(image.payload_size() > 64 * 1024);
    assert!(image.element_count() > 0);
}
