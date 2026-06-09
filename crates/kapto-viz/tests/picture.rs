// SPDX-License-Identifier: Apache-2.0

//! Pictures taken out of each kind of camera frame: compressed ones byte
//! for byte, raw ones encoded as JPEG that an independent decoder reads
//! back at their size and colour, and frames that can't be shown saying
//! why, whatever their bytes.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use kapto_config::PixelEncoding;
use kapto_viz::picture::{CameraKind, PictureFormat, Pictures, Unshown};
use proptest::prelude::*;

/// Writes a message as CDR, little-endian, as ROS 2 serialises it.
struct Cdr(Vec<u8>);

impl Cdr {
    fn new() -> Self {
        Self(vec![0, 1, 0, 0])
    }

    fn align(&mut self, to: usize) {
        while !(self.0.len() - 4).is_multiple_of(to) {
            self.0.push(0);
        }
    }

    fn u32(mut self, value: u32) -> Self {
        self.align(4);
        self.0.extend(value.to_le_bytes());
        self
    }

    fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    fn string(self, text: &str) -> Self {
        let mut cdr = self.u32(u32::try_from(text.len() + 1).unwrap());
        cdr.0.extend(text.as_bytes());
        cdr.0.push(0);
        cdr
    }

    fn bytes(self, data: &[u8]) -> Self {
        let mut cdr = self.u32(u32::try_from(data.len()).unwrap());
        cdr.0.extend(data);
        cdr
    }

    fn header(self) -> Self {
        self.u32(1_772_700_400).u32(500).string("camera_optical")
    }
}

fn compressed_image(format: &str, data: &[u8]) -> Vec<u8> {
    Cdr::new().header().string(format).bytes(data).0
}

fn image(width: u32, height: u32, encoding: &str, step: u32, data: &[u8]) -> Vec<u8> {
    Cdr::new()
        .header()
        .u32(height)
        .u32(width)
        .string(encoding)
        .u8(0)
        .u32(step)
        .bytes(data)
        .0
}

/// A small JPEG, as a camera's driver would compress it.
fn a_jpeg() -> Vec<u8> {
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 90)
        .encode(&[200; 16 * 8 * 3], 16, 8, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    jpeg
}

/// The pixels of a JPEG, decoded independently, with its size.
fn decoded(jpeg: &[u8]) -> (Vec<u8>, (u16, u16)) {
    let mut decoder = jpeg_decoder::Decoder::new(jpeg);
    let pixels = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    (pixels, (info.width, info.height))
}

#[test]
fn a_compressed_pictures_jpeg_or_png_is_shown_byte_for_byte() {
    let mut pictures = Pictures::default();
    let jpeg = a_jpeg();
    let frame = compressed_image("jpeg", &jpeg);
    let picture = pictures
        .picture(&CameraKind::CompressedImage, &frame)
        .unwrap();
    assert_eq!(
        (picture.format, picture.size, picture.bytes),
        (PictureFormat::Jpeg, None, jpeg.as_slice())
    );
    // Told by its bytes, whatever the message calls it.
    let png = b"\x89PNG\r\n\x1a\n and the rest";
    let frame = compressed_image("rgb8; png compressed bgr8", png);
    let picture = pictures
        .picture(&CameraKind::CompressedImage, &frame)
        .unwrap();
    assert_eq!(
        (picture.format, picture.bytes),
        (PictureFormat::Png, &png[..])
    );
    // A TCP camera's message is the picture, after its framing's header.
    let mut framed = u32::try_from(jpeg.len()).unwrap().to_le_bytes().to_vec();
    framed.extend(&jpeg);
    let picture = pictures
        .picture(&CameraKind::Jpeg { skip: 4 }, &framed)
        .unwrap();
    assert_eq!(picture.bytes, jpeg.as_slice());
}

#[test]
fn a_raw_ros_2_image_is_encoded_as_a_jpeg_of_its_size_and_colour() {
    let mut pictures = Pictures::default();
    for (encoding, pixel, rgb) in [
        ("rgb8", vec![250, 10, 10], [250, 10, 10]),
        ("bgr8", vec![10, 10, 250], [250, 10, 10]),
        ("rgba8", vec![10, 250, 10, 0], [10, 250, 10]),
        ("bgra8", vec![250, 10, 10, 255], [10, 10, 250]),
        ("mono8", vec![128], [128, 128, 128]),
    ] {
        let (width, height) = (64_u32, 48_u32);
        let data = pixel.repeat(64 * 48);
        let step = width * u32::try_from(pixel.len()).unwrap();
        let frame = image(width, height, encoding, step, &data);
        let picture = pictures.picture(&CameraKind::Image, &frame).unwrap();
        assert_eq!(picture.format, PictureFormat::Jpeg);
        assert_eq!(picture.size, Some((64, 48)));
        let (pixels, size) = decoded(picture.bytes);
        assert_eq!(size, (64, 48), "{encoding}");
        // Grey decodes to one channel, colour to three.
        let first: Vec<u8> = pixels
            .iter()
            .take(if encoding == "mono8" { 1 } else { 3 })
            .copied()
            .collect();
        let expected = if encoding == "mono8" {
            &rgb[..1]
        } else {
            &rgb[..]
        };
        for (got, want) in first.iter().zip(expected) {
            assert!(
                got.abs_diff(*want) <= 8,
                "{encoding}: {first:?} for {rgb:?}"
            );
        }
    }
}

#[test]
fn rows_padded_past_their_pixels_are_encoded_without_the_padding() {
    let mut pictures = Pictures::default();
    // 10 grey pixels a row, padded to 16 bytes with white the encoder must
    // never see.
    let mut data = Vec::new();
    for _ in 0..8 {
        data.extend([40; 10]);
        data.extend([255; 6]);
    }
    let frame = image(10, 8, "mono8", 16, &data);
    let picture = pictures.picture(&CameraKind::Image, &frame).unwrap();
    let (pixels, size) = decoded(picture.bytes);
    assert_eq!(size, (10, 8));
    assert!(
        pixels.iter().all(|&grey| grey.abs_diff(40) <= 8),
        "{pixels:?}"
    );
}

#[test]
fn a_raw_tcp_cameras_pixels_are_encoded_as_its_declaration_says() {
    let mut pictures = Pictures::default();
    let kind = CameraKind::Raw {
        width: 32,
        height: 16,
        encoding: PixelEncoding::Bgr8,
        skip: 0,
    };
    let frame = [0, 0, 255].repeat(32 * 16);
    let picture = pictures.picture(&kind, &frame).unwrap();
    let (pixels, size) = decoded(picture.bytes);
    assert_eq!(size, (32, 16));
    assert!(
        pixels[0] > 240 && pixels[1] < 16 && pixels[2] < 16,
        "{:?}",
        &pixels[..3]
    );
}

#[test]
fn frames_that_cant_be_shown_say_why() {
    let mut pictures = Pictures::default();
    let mut why = |kind: CameraKind, frame: &[u8]| pictures.picture(&kind, frame).unwrap_err();
    assert_eq!(
        why(
            CameraKind::CompressedImage,
            &compressed_image("tiff", b"II*\0")
        ),
        Unshown::Format("tiff".into())
    );
    assert_eq!(
        why(CameraKind::Jpeg { skip: 0 }, b"not a picture"),
        Unshown::Format("jpeg".into())
    );
    assert_eq!(
        why(CameraKind::Image, &image(2, 2, "yuv422", 4, &[0; 8])),
        Unshown::Encoding("yuv422".into())
    );
    assert_eq!(
        why(CameraKind::Image, &image(4, 4, "rgb8", 12, &[0; 40])),
        Unshown::Short {
            needed: 48,
            has: 40
        }
    );
    assert_eq!(
        why(
            CameraKind::Image,
            &image(70_000, 1, "mono8", 70_000, &vec![0; 70_000])
        ),
        Unshown::TooLarge(70_000, 1)
    );
    assert_eq!(
        why(
            CameraKind::Image,
            &compressed_image("jpeg", &a_jpeg())[..20]
        ),
        Unshown::Unreadable("cut short")
    );
    assert_eq!(
        why(CameraKind::CompressedImage, &[0, 7, 0, 0, 1, 2]),
        Unshown::Unreadable("not plain CDR")
    );
    // What the live view names it as.
    assert_eq!(
        Unshown::Encoding("yuv422".into()).to_string(),
        "raw pictures in `yuv422`, which the live view can't show: only rgb8, bgr8, rgba8, bgra8 and mono8"
    );
}

#[test]
fn a_big_endian_image_is_read_too() {
    let mut frame = vec![0, 0, 0, 0];
    let mut put = |value: u32| frame.extend(value.to_be_bytes());
    put(0);
    put(0);
    put(1);
    frame.extend([0, 0, 0, 0]);
    // Height, width, then the encoding.
    frame.extend(2_u32.to_be_bytes());
    frame.extend(2_u32.to_be_bytes());
    frame.extend(6_u32.to_be_bytes());
    frame.extend(b"mono8\0");
    // `is_bigendian`, then padding to the step's four bytes.
    frame.extend([0, 0]);
    frame.extend(2_u32.to_be_bytes());
    frame.extend(4_u32.to_be_bytes());
    frame.extend([9, 9, 9, 9]);
    let mut pictures = Pictures::default();
    let picture = pictures.picture(&CameraKind::Image, &frame).unwrap();
    assert_eq!(picture.size, Some((2, 2)));
}

proptest! {
    #[test]
    fn any_bytes_are_shown_or_said_why_never_a_panic(frame in proptest::collection::vec(any::<u8>(), 0..512)) {
        let mut pictures = Pictures::default();
        for kind in [
            CameraKind::CompressedImage,
            CameraKind::Image,
            CameraKind::Jpeg { skip: 4 },
            CameraKind::Raw { width: 4, height: 4, encoding: PixelEncoding::Rgb8, skip: 4 },
        ] {
            let _ = pictures.picture(&kind, &frame);
        }
    }

    #[test]
    fn a_compressed_picture_round_trips_whatever_its_header(
        stamp in any::<u32>(),
        frame_id in "[a-z_]{0,20}",
        rest in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let mut jpeg = vec![0xff, 0xd8, 0xff];
        jpeg.extend(&rest);
        let frame = Cdr::new().u32(stamp).u32(0).string(&frame_id).string("jpeg").bytes(&jpeg).0;
        let mut pictures = Pictures::default();
        let picture = pictures.picture(&CameraKind::CompressedImage, &frame).unwrap();
        prop_assert_eq!(picture.bytes, jpeg.as_slice());
    }
}

#[test]
fn a_tcp_cameras_pictures_start_after_its_length_prefix_but_a_fixed_frame_is_all_picture() {
    let declared = |framing: &str, camera: &str| {
        let file = kapto_config::ConfigFile {
            path: "streams.toml".into(),
            contents: format!(
                "[streams.front]\nmax_message_bytes = 2000000\n\
                 tcp = {{ listen = \"0.0.0.0:7010\", framing = {framing} }}\ncamera = {camera}\n"
            ),
        };
        CameraKind::of(&kapto_config::parse_streams(&file).unwrap()[0]).unwrap()
    };
    assert_eq!(
        declared(
            "{ kind = \"length_prefix\", length_bytes = 4 }",
            "{ format = \"jpeg\" }"
        ),
        CameraKind::Jpeg { skip: 4 }
    );
    assert_eq!(
        declared(
            "{ kind = \"length_prefix\", length_bytes = 2, header_bytes = 6 }",
            "{ format = \"jpeg\" }"
        ),
        CameraKind::Jpeg { skip: 6 }
    );
    assert_eq!(
        declared(
            "{ kind = \"fixed\", size = 12 }",
            "{ format = \"raw\", width = 2, height = 2, encoding = \"rgb8\" }"
        ),
        CameraKind::Raw {
            width: 2,
            height: 2,
            encoding: PixelEncoding::Rgb8,
            skip: 0
        }
    );
}
