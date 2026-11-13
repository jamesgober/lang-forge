//! Memory bounds, measured: a counting allocator records the peak heap use
//! of forging and parsing, so the limits the crate claims are checked rather
//! than assumed. Each test holds a lock while it measures, so no two run at
//! once.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use lang_forge::Language;

/// The system allocator, counting the bytes in use and their peak.
struct Counting;

static IN_USE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static MEASURING: Mutex<()> = Mutex::new(());

fn grew(bytes: usize) {
    let now = IN_USE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let _ = PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are the caller's; the counters are
// plain atomics and never affect what is allocated.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract for
        // `layout`, which is `System.alloc`'s contract too.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            grew(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // SAFETY: `block` came from this allocator, so from `System`, with
        // this `layout`, as the caller guarantees.
        unsafe { System.dealloc(block, layout) };
        let _ = IN_USE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: `block` came from `System` with `layout`, and `size` meets
        // `realloc`'s requirements, as the caller guarantees.
        let moved = unsafe { System.realloc(block, layout, size) };
        if !moved.is_null() {
            if size > layout.size() {
                grew(size - layout.size());
            } else {
                let _ = IN_USE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `work` and returns its result and the most heap it held at once.
fn peak<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let base = IN_USE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let out = work();
    (out, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

const MIB: usize = 1 << 20;

#[test]
fn test_memo_memory_does_not_grow_with_nesting_depth() {
    let _lock = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    // Every statement speculates, and the alternative tried first fails only
    // after its expression — which holds the whole block below — is parsed.
    let lang = Language::from_lsf(
        "[language]\nname = \"s\"\n[rules]\nfile = \"stmt*\"\nstmt = \"assign | expr ';'\"\n\
         assign = \"expr '=' expr ';'\"\nblock = \"'{' stmt* '}'\"\n\
         call = \"IDENT '(' (expr (',' expr)*)? ')'\"\n\
         [rules.expr]\noperand = \"call | IDENT | NUMBER | block\"\nlevels = [{ left = [\"+\"] }]\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    // The same 12,000 statements, flat and nested 60 blocks deep.
    let source = |depth: usize| {
        let mut src = String::new();
        for _ in 0..depth {
            src.push_str("{ ");
            for i in 0..12_000 / depth {
                src.push_str(&format!("f(x{i}) + 1; "));
            }
        }
        src.push_str("x;");
        src.push_str(&" };".repeat(depth));
        src
    };
    let mut peaks = [0; 2];
    for (slot, depth) in peaks.iter_mut().zip([1, 60]) {
        let src = source(depth);
        let (errors, bytes) = peak(|| lang.parse(&src).has_errors());
        assert!(!errors);
        *slot = bytes;
    }
    // 1.0.0 needed about seven times the flat peak at this depth (the memo
    // copied every level's events once per enclosing level).
    assert!(
        peaks[1] < peaks[0] * 3 / 2,
        "flat {} MiB, nested {} MiB",
        peaks[0] / MIB,
        peaks[1] / MIB
    );
}

#[test]
fn test_hostile_schematic_is_refused_in_bounded_memory() {
    let _lock = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    // 30,000 keywords and 100,000 two-way choices among them, about 2.4 MB:
    // the token sets would have needed several gigabytes.
    let mut text = String::from("[language]\nname = \"h\"\n[rules]\n");
    for r in 0..100 {
        text.push_str(&format!("r{r} = \""));
        for g in r * 1000..(r + 1) * 1000 {
            let (a, b) = ((2 * g) % 30_000, (2 * g + 1) % 30_000);
            text.push_str(&format!("('k{a}' | 'k{b}')? "));
        }
        text.push_str("\"\n");
    }
    let (refused, bytes) = peak(|| Language::from_lsf(&text).is_err());
    assert!(refused);
    assert!(bytes < 128 * MIB, "{} MiB", bytes / MIB);
}

#[test]
fn test_large_keyword_grammar_forges_within_the_table_budget() {
    let _lock = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    // 30,000 keywords, each its own alternative: a legitimate if extreme
    // grammar, which forged in 1.0.0 using about 480 MiB.
    let alternatives: Vec<String> = (0..30_000).map(|i| format!("'k{i}' 'z'")).collect();
    let text = format!(
        "[language]\nname = \"k\"\n[rules]\na = \"{}\"\n",
        alternatives.join(" | ")
    );
    let (lang, bytes) = peak(|| Language::from_lsf(&text));
    let lang = lang.unwrap_or_else(|e| panic!("{e}"));
    assert!(!lang.parse("k12345 z").has_errors());
    assert!(bytes < 256 * MIB, "{} MiB", bytes / MIB);
}

/// FNV-1a, the image body's checksum.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[test]
fn test_forged_image_count_reserves_no_more_than_the_image() {
    // A count of capabilities as large as the bytes after it, each of which
    // encodes in a byte at least but is far larger in memory. Loading fails;
    // it used to reserve 32 times the image first. What it holds now is the
    // items it decodes before the error, a small multiple of the image.
    let pad = 4usize << 20;
    let mut body = Vec::with_capacity(pad + 32);
    body.extend_from_slice(&1u32.to_le_bytes());
    body.push(b'x'); // name "x"
    body.push(0); // no version
    body.extend_from_slice(&0u32.to_le_bytes()); // no extensions
    body.extend_from_slice(&(pad as u32).to_le_bytes()); // capabilities
    body.resize(body.len() + pad, 0);
    let mut image = Vec::new();
    image.extend_from_slice(b"LSL\0");
    image.extend_from_slice(&1u16.to_le_bytes());
    image.extend_from_slice(&0u16.to_le_bytes());
    image.extend_from_slice(&(body.len() as u64).to_le_bytes());
    image.extend_from_slice(&fnv1a(&body).to_le_bytes());
    image.extend_from_slice(&body);
    let (result, used) = peak(|| Language::from_image(&image).is_err());
    assert!(result, "a forged image is refused");
    assert!(
        used <= 8 * image.len(),
        "{used} bytes to refuse an image of {}",
        image.len()
    );
}
