/// App mark: a forward double-chevron (amber over white) on a deep ink rounded tile. Returns RGBA pixels.
pub fn app_icon(size: u32) -> Vec<u8> {
    let s = size as f32;
    let r = s * 0.22;
    let stroke = s * 0.1;
    let top = [(0.28 * s, 0.5 * s), (0.5 * s, 0.32 * s), (0.72 * s, 0.5 * s)];
    let bot = [(0.28 * s, 0.68 * s), (0.5 * s, 0.5 * s), (0.72 * s, 0.68 * s)];
    let seg = |px: f32, py: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((px - (ax + t * dx)).powi(2) + (py - (ay + t * dy)).powi(2)).sqrt()
    };
    let dmin = |x: f32, y: f32, p: &[(f32, f32); 3]| seg(x, y, p[0], p[1]).min(seg(x, y, p[1], p[2]));
    let bg = [22.0, 26.0, 34.0];
    let amber = [245.0, 166.0, 35.0];
    let white = [236.0, 238.0, 242.0];
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for yi in 0..size {
        for xi in 0..size {
            let (x, y) = (xi as f32 + 0.5, yi as f32 + 0.5);
            let cx = x.clamp(r, s - r);
            let cy = y.clamp(r, s - r);
            if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() > r {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let a_bot = (stroke / 2.0 + 0.5 - dmin(x, y, &bot)).clamp(0.0, 1.0);
            let a_top = (stroke / 2.0 + 0.5 - dmin(x, y, &top)).clamp(0.0, 1.0);
            // The amber (upper) chevron wins where the two overlap.
            let col = if a_top > 0.0 { amber } else { white };
            let a = a_bot.max(a_top);
            for c in 0..3 {
                out.push((bg[c] + (col[c] - bg[c]) * a) as u8);
            }
            out.push(255);
        }
    }
    out
}
