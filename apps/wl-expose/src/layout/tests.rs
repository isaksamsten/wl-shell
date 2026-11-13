use super::*;

const EPS: f64 = 1e-6;
const MAX_ITEMS: usize = 40;

fn item(w: f64, h: f64) -> Item {
    Item { w, h }
}

fn overlaps(a: Rect, b: Rect, gap: f64) -> bool {
    a.x < b.x + b.w + gap - EPS
        && b.x < a.x + a.w + gap - EPS
        && a.y < b.y + b.h + gap - EPS
        && b.y < a.y + a.h + gap - EPS
}

/// Nothing empty, everything inside the area, aspect ratios kept, no
/// upscaling past `max_scale`, and at least `gap` between thumbnails.
fn check_invariants(
    name: &str,
    items: &[Item],
    area: Rect,
    max_scale: f64,
    gap: f64,
    out: &[Rect],
) {
    check_footprints(name, items, area, max_scale, gap, 0.0, out);
}

/// Like `check_invariants`, for footprints: each thumbnail plus `below`
/// under it is inside the area and at least `gap` from the others.
fn check_footprints(
    name: &str,
    items: &[Item],
    area: Rect,
    max_scale: f64,
    gap: f64,
    below: f64,
    out: &[Rect],
) {
    assert_eq!(items.len(), out.len(), "{name}: wrong rect count");
    let foot = |r: &Rect| Rect::new(r.x, r.y, r.w, r.h + below);
    for (i, (it, r)) in items.iter().zip(out).enumerate() {
        assert!(r.w > 0.0 && r.h > 0.0, "{name}: item {i} has empty rect");
        let f = foot(r);
        assert!(
            f.x >= area.x - EPS
                && f.y >= area.y - EPS
                && f.x + f.w <= area.x + area.w + EPS
                && f.y + f.h <= area.y + area.h + EPS,
            "{name}: item {i} outside area ({:.2},{:.2} {:.2}x{:.2})",
            f.x,
            f.y,
            f.w,
            f.h
        );
        let want = it.w / it.h;
        assert!(
            (r.w / r.h - want).abs() < 1e-6 * want,
            "{name}: item {i} aspect {:.6}, want {want:.6}",
            r.w / r.h
        );
        if max_scale > 0.0 {
            assert!(
                r.w <= it.w * max_scale + EPS,
                "{name}: item {i} upscaled ({:.2} > {:.2})",
                r.w,
                it.w
            );
        }
        for (j, other) in out.iter().enumerate().skip(i + 1) {
            assert!(
                !overlaps(f, foot(other), gap),
                "{name}: items {i} and {j} too close"
            );
        }
    }
}

fn thumbnail_area(rects: &[Rect]) -> f64 {
    rects.iter().map(|r| r.w * r.h).sum()
}

/// Small deterministic PRNG (xorshift64*), so the tests need no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct RandomCase {
    name: String,
    items: Vec<Item>,
    area: Rect,
    params: Params,
}

fn random_cases() -> impl Iterator<Item = RandomCase> {
    let mut rng = Rng(42);
    (0..2000).map(move |iter| {
        let n = 1 + rng.below(MAX_ITEMS as u64) as usize;
        let items = (0..n)
            .map(|_| item(rng.range(50.0, 3000.0), rng.range(50.0, 2000.0)))
            .collect();
        let area = Rect::new(
            rng.range(0.0, 100.0),
            rng.range(0.0, 100.0),
            rng.range(400.0, 3840.0),
            rng.range(300.0, 2160.0),
        );
        let params = Params {
            spacing: rng.range(0.0, 40.0),
            max_scale: if rng.below(4) == 0 {
                0.0
            } else {
                rng.range(0.2, 2.0)
            },
            below: 0.0,
        };
        RandomCase {
            name: format!("random#{iter}"),
            items,
            area,
            params,
        }
    })
}

#[test]
fn random() {
    for (iter, c) in random_cases().enumerate() {
        let RandomCase {
            name,
            items,
            area,
            params: p,
        } = &c;
        // Random sizes include cramped cases where spacing has to shrink,
        // so only check that nothing overlaps.
        let out = compute(items, *area, p);
        check_invariants(name, items, *area, p.max_scale, 0.0, &out);

        // A requested gap that fits the natural ordering must be kept. When
        // it doesn't fit, compute() reduces it (documented, with a warning).
        let natural_fits = Seed::natural(items, area.w / area.h).gap_fits(p.spacing, 0.0, *area);
        if natural_fits {
            check_invariants(name, items, *area, p.max_scale, p.spacing, &out);
        }
        if iter < 50 {
            assert_eq!(
                out,
                compute(items, *area, p),
                "{name}: layout is not deterministic"
            );
        }
    }
}

/// FNV-1a over the bits of every rect, so exact output changes are caught.
fn fingerprint(rects: &[Rect], hash: &mut u64) {
    for r in rects {
        for v in [r.x, r.y, r.w, r.h] {
            for b in v.to_bits().to_le_bytes() {
                *hash = (*hash ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
    }
}

#[test]
fn snapshot() {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for c in random_cases().take(300) {
        fingerprint(&compute(&c.items, c.area, &c.params), &mut hash);
    }
    assert_eq!(hash, 0x4fb1_0a08_3fc8_8b59, "layout output changed");
}

#[test]
fn below_footprints() {
    let mut rng = Rng(7);
    for (iter, mut c) in random_cases().enumerate().take(1000) {
        let below = rng.range(0.0, 120.0);
        c.params.below = below;
        let p = &c.params;
        let name = format!("{} below {below:.1}", c.name);
        let out = compute(&c.items, c.area, p);
        let seed = Seed::natural(&c.items, c.area.w / c.area.h);
        if seed.gap_fits(p.spacing, below, c.area) {
            check_footprints(&name, &c.items, c.area, p.max_scale, p.spacing, below, &out);
        } else if seed.gap_fits(0.0, below, c.area) {
            // The gap is reduced, the space below is kept.
            check_footprints(&name, &c.items, c.area, p.max_scale, 0.0, below, &out);
        } else {
            check_invariants(&name, &c.items, c.area, p.max_scale, 0.0, &out);
        }
        if iter < 50 {
            assert_eq!(
                out,
                compute(&c.items, c.area, p),
                "{name}: not deterministic"
            );
        }
    }
}

#[test]
fn below_zero_is_unchanged() {
    // Negative is treated as 0, which is exactly the plain layout.
    for mut c in random_cases().take(200) {
        let plain = compute(&c.items, c.area, &c.params);
        c.params.below = -5.0;
        assert_eq!(plain, compute(&c.items, c.area, &c.params), "{}", c.name);
    }
}

#[test]
fn below_realistic() {
    // Room for an icon and a title under each thumbnail; gaps stay exact,
    // and the space below costs thumbnail size rather than overlapping.
    let items = [
        item(1920.0, 1080.0),
        item(1280.0, 800.0),
        item(800.0, 600.0),
        item(1600.0, 1000.0),
    ];
    let area = Rect::new(48.0, 48.0, 1824.0, 984.0);
    let mut p = Params {
        spacing: 24.0,
        max_scale: 1.0,
        below: 0.0,
    };
    let plain = compute(&items, area, &p);
    p.below = 60.0;
    let out = compute(&items, area, &p);
    check_footprints("below realistic", &items, area, 1.0, 24.0, 60.0, &out);
    assert!(thumbnail_area(&out) < thumbnail_area(&plain));

    // A single window: the footprint is centered and fills the height.
    let one = compute(&items[..1], area, &p)[0];
    check_footprints("below single", &items[..1], area, 1.0, 0.0, 60.0, &[one]);
    let foot_center = one.y + (one.h + 60.0) / 2.0;
    assert!((foot_center - area.center_y()).abs() < EPS, "{one:?}");
    assert!(
        (one.y + one.h + 60.0 - (area.y + area.h)).abs() < EPS,
        "not fitted: {one:?}"
    );
}

#[test]
fn below_too_large() {
    // More space below than the area has: reduced, never outside the area.
    let area = Rect::new(0.0, 0.0, 800.0, 200.0);
    let p = Params {
        spacing: 24.0,
        max_scale: 1.0,
        below: 500.0,
    };
    let out = compute(&UTILITIES, area, &p);
    check_invariants("below too large", &UTILITIES, area, 1.0, 0.0, &out);
}

#[test]
fn realistic() {
    // Typical desktop: thumbnail gaps are kept exactly.
    let items = [
        item(1920.0, 1080.0),
        item(1920.0, 1080.0),
        item(960.0, 1080.0),
        item(1280.0, 800.0),
        item(800.0, 600.0),
        item(800.0, 600.0),
        item(1600.0, 1000.0),
        item(3840.0, 2160.0),
        item(500.0, 900.0),
    ];
    let area = Rect::new(48.0, 48.0, 1824.0, 984.0);
    let p = Params {
        spacing: 24.0,
        max_scale: 1.0,
        below: 0.0,
    };
    for n in 1..=items.len() {
        let out = compute(&items[..n], area, &p);
        check_invariants(
            &format!("realistic/{n}"),
            &items[..n],
            area,
            1.0,
            p.spacing,
            &out,
        );
    }
}

#[test]
fn single_small() {
    // A small window is centered at its native size, never upscaled.
    let area = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    let p = Params {
        spacing: 20.0,
        max_scale: 1.0,
        below: 0.0,
    };
    let out = compute(&[item(400.0, 300.0)], area, &p)[0];
    assert!(
        (out.w - 400.0).abs() < EPS && (out.h - 300.0).abs() < EPS,
        "size {out:?}"
    );
    assert!(
        (out.x - 760.0).abs() < EPS && (out.y - 390.0).abs() < EPS,
        "pos {out:?}"
    );
}

#[test]
fn single_large() {
    // A large window fills the area along its limiting axis.
    let area = Rect::new(0.0, 0.0, 1920.0, 1200.0);
    let p = Params {
        spacing: 20.0,
        max_scale: 1.0,
        below: 0.0,
    };
    let out = compute(&[item(3840.0, 2160.0)], area, &p)[0];
    assert!((out.w - 1920.0).abs() < EPS, "width {}", out.w);
    assert!((out.y - (1200.0 - 1080.0) / 2.0).abs() < EPS, "y {}", out.y);
}

#[test]
fn freeform() {
    // Two large windows and two utilities: invariants hold, and translating
    // the area must not alter the sizes or relative composition.
    let items = [
        item(1600.0, 1200.0),
        item(400.0, 300.0),
        item(1600.0, 1200.0),
        item(400.0, 300.0),
    ];
    let mut area = Rect::new(48.0, 48.0, 1824.0, 984.0);
    let p = Params {
        spacing: 24.0,
        max_scale: 1.0,
        below: 0.0,
    };
    let out = compute(&items, area, &p);
    check_invariants("freeform", &items, area, 1.0, p.spacing, &out);

    area.x += 123.0;
    area.y -= 57.0;
    let shifted = compute(&items, area, &p);
    for (i, (s, o)) in shifted.iter().zip(&out).enumerate() {
        assert!(
            (s.x - o.x - 123.0).abs() < EPS
                && (s.y - o.y + 57.0).abs() < EPS
                && (s.w - o.w).abs() < EPS
                && (s.h - o.h).abs() < EPS,
            "translation changed item {i}"
        );
    }
}

const UTILITIES: [Item; 5] = [
    Item { w: 400.0, h: 300.0 },
    Item { w: 600.0, h: 500.0 },
    Item { w: 350.0, h: 700.0 },
    Item { w: 500.0, h: 400.0 },
    Item { w: 800.0, h: 600.0 },
];

#[test]
fn freeform_limits() {
    let area = Rect::new(48.0, 48.0, 1824.0, 984.0);
    let mut p = Params {
        spacing: 24.0,
        max_scale: 1.0,
        below: 0.0,
    };
    let out = compute(&UTILITIES, area, &p);
    check_invariants("freeform capped", &UTILITIES, area, 1.0, 24.0, &out);

    p.spacing = -10.0;
    p.max_scale = 0.0;
    let out = compute(&UTILITIES, area, &p);
    check_invariants("freeform uncapped", &UTILITIES, area, 0.0, 0.0, &out);

    // An impossible requested gap is reduced rather than overflowing.
    p.spacing = 500.0;
    let mut area = Rect::new(5.0, 10.0, 30.0, 20.0);
    let out = compute(&UTILITIES, area, &p);
    check_invariants("freeform cramped", &UTILITIES, area, 0.0, 0.0, &out);

    area.w = 0.0;
    for r in compute(&UTILITIES, area, &p) {
        assert_eq!(
            r,
            Rect::new(5.0, 20.0, 0.0, 0.0),
            "empty area did not collapse"
        );
    }
}

// Stale since the natural-placement layout reserves 10% breathing room;
// this already failed in the C test suite.
#[test]
#[ignore = "stale: natural placement reserves breathing room instead of growing to the cap"]
fn freeform_utilities_grow_to_cap() {
    let area = Rect::new(48.0, 48.0, 1824.0, 984.0);
    let p = Params {
        spacing: 24.0,
        max_scale: 1.0,
        below: 0.0,
    };
    let out = compute(&UTILITIES, area, &p);
    for (i, (r, it)) in out.iter().zip(&UTILITIES).enumerate() {
        assert!(
            (r.w - it.w).abs() < EPS,
            "utility {i} did not grow to its cap"
        );
    }
}

#[test]
fn neighbors() {
    // 3 on top, 2 below (centered)
    let r = [
        Rect::new(0.0, 0.0, 100.0, 100.0),
        Rect::new(120.0, 0.0, 100.0, 100.0),
        Rect::new(240.0, 0.0, 100.0, 100.0),
        Rect::new(60.0, 120.0, 100.0, 100.0),
        Rect::new(180.0, 120.0, 100.0, 100.0),
    ];
    assert_eq!(neighbor(&r, 0, Dir::Right), Some(1));
    assert_eq!(neighbor(&r, 1, Dir::Left), Some(0));
    assert_eq!(neighbor(&r, 0, Dir::Left), None);
    assert_eq!(neighbor(&r, 0, Dir::Up), None);
    assert_eq!(neighbor(&r, 0, Dir::Down), Some(3));
    assert_eq!(neighbor(&r, 2, Dir::Down), Some(4));
    assert!(matches!(neighbor(&r, 4, Dir::Up), Some(1 | 2)));
    assert_eq!(neighbor(&r, 3, Dir::Right), Some(4));
    assert!(matches!(neighbor(&r, 4, Dir::Right), None | Some(2)));
    assert_eq!(neighbor(&r, 7, Dir::Right), None);
}

#[test]
fn degenerate() {
    // Zero sizes are treated as 1x1, a tiny area doesn't crash.
    let items = [item(0.0, 0.0), item(-5.0, 10.0)];
    let area = Rect::new(0.0, 0.0, 10.0, 10.0);
    let p = Params {
        spacing: 50.0,
        max_scale: 1.0,
        below: 0.0,
    };
    assert_eq!(compute(&items, area, &p).len(), 2);
    assert!(compute(&[], area, &p).is_empty());
}
