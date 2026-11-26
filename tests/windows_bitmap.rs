#[path = "../src/platform/windows/bitmap.rs"]
mod bitmap;

use clipserve::capture::encode_png;

#[test]
fn windows_clipboard_bitmap_preserves_alpha_and_uses_top_down_bgra() {
    let png = encode_png(2, 1, &[1, 2, 3, 4, 255, 128, 200, 0]).unwrap();
    let dib = bitmap::dibv5(&png).unwrap();
    assert_eq!(&dib[8..12], &(-1_i32).to_le_bytes());
    assert_eq!(&dib[124..], &[3, 2, 1, 4, 200, 128, 255, 0]);
}
