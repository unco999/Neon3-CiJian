//! Font metrics diagnostic probe.
use fontdue::Font;

fn main() {
    let font_bytes = include_bytes!("../../assets/fonts/SarasaUiSC-Light.ttf");
    let font = Font::from_bytes(font_bytes as &[u8], fontdue::FontSettings::default()).unwrap();

    let size = 16.0;
    let lm = font.horizontal_line_metrics(size).unwrap();
    println!("Font raster size: {}", size);
    println!("Ascent: {}", lm.ascent);
    println!("Descent: {}", lm.descent);
    println!("Line height (new_line_size): {}", lm.new_line_size);
    println!();

    let test_chars = vec!['a', 'b', '+', '_', '-', '=', '0', 'A', 'g', 'p', 'y', 'j'];

    println!("{:<6} {:>8} {:>8} {:>8} {:>8} {:>10} {:>10}", "char", "xmin", "ymin", "width", "height", "advance", "plane_min_y");
    println!("{}", "-".repeat(70));

    for ch in test_chars {
        let metrics = font.metrics(ch, size);
        let plane_min_y = -metrics.bounds.height - metrics.bounds.ymin;
        println!("{:<6} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>10.2} {:>10.2}",
            ch,
            metrics.xmin,
            metrics.bounds.ymin,
            metrics.bounds.width,
            metrics.bounds.height,
            metrics.advance_width,
            plane_min_y
        );
    }
}
