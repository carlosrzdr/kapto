// SPDX-License-Identifier: Apache-2.0

//! Any bytes read as a camera's frame: the first bytes pick the kind of
//! camera (a ROS 2 compressed or raw image, a TCP camera's JPEG or raw
//! pictures, with the header its framing leaves before them), the rest is
//! the frame. A picture found is always inside the frame.

#![no_main]

use kapto_decode::camera::{CameraKind, Frame, PictureFormat, PixelEncoding, frame};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let Ok(kind) = kind(&mut input) else {
        return;
    };
    let bytes = input.take_rest();
    let inside = |data: &[u8]| {
        let start = data.as_ptr() as usize;
        let frame = bytes.as_ptr() as usize;
        start >= frame && start + data.len() <= frame + bytes.len()
    };
    match frame(&kind, bytes) {
        Ok(Frame::Compressed { format, data }) => {
            assert!(inside(data));
            let magic: &[u8] = match format {
                PictureFormat::Jpeg => &[0xff, 0xd8, 0xff],
                PictureFormat::Png => b"\x89PNG\r\n\x1a\n",
            };
            assert!(data.starts_with(magic));
        }
        Ok(Frame::Raw { data, .. }) => assert!(inside(data)),
        Err(_) => {}
    }
});

fn kind(input: &mut Unstructured<'_>) -> Result<CameraKind> {
    Ok(match input.int_in_range(0..=3_u8)? {
        0 => CameraKind::CompressedImage,
        1 => CameraKind::Image,
        2 => CameraKind::Jpeg {
            skip: input.int_in_range(0..=16)?,
        },
        _ => CameraKind::Raw {
            width: input.arbitrary()?,
            height: input.arbitrary()?,
            encoding: *input.choose(&PixelEncoding::ALL)?,
            skip: input.int_in_range(0..=16)?,
        },
    })
}
