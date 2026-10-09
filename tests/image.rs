//! Language images (`.lsl`, ISSUES M12): round trips, determinism, refusal
//! of damaged or foreign bytes, and — the guarantee that matters for an
//! untrusted file — no panic from any bytes, loaded or refused.

use lang_forge::{IMAGE_FORMAT, ImageError, Language};
use proptest::prelude::*;

const SCHEMATICS: [&str; 5] = [
    include_str!("../examples/schematics/mini.lsf"),
    include_str!("../examples/schematics/calc.lsf"),
    include_str!("../examples/schematics/json.lsf"),
    include_str!("../examples/schematics/conf.lsf"),
    include_str!("sketches/mox.lsf"),
];

const SAMPLES: [&str; 6] = [
    "fn main() { let x = 1 + 2; }",
    "{\"a\": [1, 2.5, true, null]}",
    "key = value\n[section]\nn = 1\n",
    "<?mox echo \"a {$b} $c\"; ?> text <?= $x ?>",
    "1 + 2 * (3 - 4)",
    "\u{0}\u{7f}é𝄞 /* ",
];

/// FNV-1a 64, as the image header uses.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Re-seals a header after the body changed, so the decoder (not the hash)
/// judges the mutated tables.
fn reseal(image: &mut [u8]) {
    let body_len = (image.len() - 24) as u64;
    image[8..16].copy_from_slice(&body_len.to_le_bytes());
    let hash = fnv1a(&image[24..]);
    image[16..24].copy_from_slice(&hash.to_le_bytes());
}

#[test]
fn test_round_trip_every_example_and_mox() {
    for sketch in SCHEMATICS {
        let lang = Language::from_lsf(sketch).expect("forges");
        let image = lang.to_image();
        assert_eq!(&image[..4], b"LSL\0");
        assert_eq!(u16::from_le_bytes([image[4], image[5]]), IMAGE_FORMAT);
        let loaded = Language::from_image(&image).expect("its own image loads");
        assert_eq!(loaded.name(), lang.name());
        assert_eq!(loaded.format(), lang.format());
        assert_eq!(loaded.kind_count(), lang.kind_count());
        assert_eq!(loaded.to_image(), image);
        for src in SAMPLES {
            let (a, b) = (lang.parse(src), loaded.parse(src));
            assert_eq!(a.dump(), b.dump());
            assert_eq!(a.diagnostics(), b.diagnostics());
            assert_eq!(a.injections().len(), b.injections().len());
        }
        // Forging again gives the same bytes.
        assert_eq!(
            Language::from_lsf(sketch).expect("forges").to_image(),
            image
        );
    }
}

#[test]
fn test_refusals() {
    let image = Language::from_lsf(SCHEMATICS[0])
        .expect("forges")
        .to_image();
    assert_eq!(
        Language::from_image(b"").unwrap_err(),
        ImageError::NotAnImage
    );
    assert_eq!(
        Language::from_image(&image[..23]).unwrap_err(),
        ImageError::NotAnImage
    );
    let mut other = image.clone();
    other[4] = 99;
    assert_eq!(
        Language::from_image(&other).unwrap_err(),
        ImageError::Format(99)
    );
    assert!(
        ImageError::Format(99)
            .to_string()
            .contains("forge the sketch again")
    );
    let mut truncated = image.clone();
    truncated.pop();
    assert_eq!(
        Language::from_image(&truncated).unwrap_err(),
        ImageError::Corrupt
    );
    let mut reserved = image.clone();
    reserved[6] = 1;
    assert_eq!(
        Language::from_image(&reserved).unwrap_err(),
        ImageError::Corrupt
    );
    // A sealed body with trailing bytes: the decoder sees them.
    let mut trailing = image.clone();
    trailing.push(0);
    reseal(&mut trailing);
    assert_eq!(
        Language::from_image(&trailing).unwrap_err(),
        ImageError::Invalid
    );
    // A sealed, truncated body.
    let mut short = image;
    short.truncate(short.len() - 5);
    reseal(&mut short);
    assert_eq!(
        Language::from_image(&short).unwrap_err(),
        ImageError::Invalid
    );
}

/// Loads `bytes`; if they load, the language must lex and parse anything.
fn load_and_use(bytes: &[u8]) {
    if let Ok(lang) = Language::from_image(bytes) {
        for src in SAMPLES {
            let parse = lang.parse(src);
            assert_eq!(parse.tree().text(src), Some(src));
            let _ = lang.lex(src);
            let _ = parse.dump();
        }
        let _ = lang.to_image();
    }
}

/// Cases per property: 256, or `PROPTEST_CASES` for a longer soak.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// The mini and calc images, forged once.
fn images() -> &'static [Vec<u8>] {
    static IMAGES: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    IMAGES.get_or_init(|| {
        [SCHEMATICS[0], SCHEMATICS[1], SCHEMATICS[4]]
            .iter()
            .map(|s| Language::from_lsf(s).expect("forges").to_image())
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// Byte flips anywhere in a sealed body: refused or usable, never a
    /// panic.
    #[test]
    fn prop_mutated_images_never_panic(
        which in 0usize..3,
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
    ) {
        let mut image = images()[which].clone();
        let body = image.len() - 24;
        for (at, byte) in flips {
            let i = 24 + at.index(body);
            image[i] ^= byte | 1;
        }
        reseal(&mut image);
        load_and_use(&image);
    }

    /// Small integers written over the body (lengths and indices are the
    /// fields that matter): refused or usable, never a panic.
    #[test]
    fn prop_small_values_in_images_never_panic(
        which in 0usize..3,
        writes in proptest::collection::vec((any::<prop::sample::Index>(), 0u32..300), 1..6),
    ) {
        let mut image = images()[which].clone();
        let body = image.len() - 24;
        for (at, value) in writes {
            let i = 24 + at.index(body.saturating_sub(4));
            image[i..i + 4].copy_from_slice(&value.to_le_bytes());
        }
        reseal(&mut image);
        load_and_use(&image);
    }

    /// Truncations and arbitrary bytes after a valid header.
    #[test]
    fn prop_truncated_and_random_bodies_never_panic(
        which in 0usize..3,
        cut in any::<prop::sample::Index>(),
        noise in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        let mut image = images()[which].clone();
        let keep = 24 + cut.index(image.len() - 24);
        image.truncate(keep);
        image.extend_from_slice(&noise);
        reseal(&mut image);
        load_and_use(&image);
    }
}
