#![cfg(test)]
//! The account avatar renders from bytes the account runtime fetched, so the
//! desktop installs no HTTP client for GPUI's image loader.

use aeris_account_runtime::ProfilePhoto;
use aeris_desktop::account::AccountAvatar;
use gpui::{DevicePixels, SvgRenderer, size};
use std::sync::Arc;

const GIF_1X1: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

#[test]
fn runtime_photo_bytes_decode_into_the_avatar_image() {
    let photo = ProfilePhoto::from_bytes(GIF_1X1.to_vec()).expect("runtime accepts the GIF");
    let rendered = AccountAvatar::new(photo)
        .image()
        .to_image_data(SvgRenderer::new(Arc::new(())))
        .expect("GPUI decodes the avatar bytes");
    assert_eq!(rendered.size(0), size(DevicePixels(1), DevicePixels(1)));
}
