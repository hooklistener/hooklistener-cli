use std::time::Duration;

use tokio::sync::watch;

const LOGO: &[&str] = &[
    "                            **%%%%%%%%%%%%*",
    "                         *%%%%%%%%%%%%%%%%%%%%*",
    "                      *%%%%%%%%%%%%%%%%%%%%%%%%%*",
    "                     %%%%%%%%%%%%%%%%%%%%%%%%%%%%%*",
    "                   *%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%",
    "                  *%%%%%%%%%%%%**      *%%%%%%%%%%%%%",
    "                 %%%%%%%%%%%*             *%%%%%%%%%%%",
    "               *%%%%%%%%%%%                 %%%%%%%%%%*",
    "  ***********%%%%%%%%%%%%%                   %%%%%%%%%%",
    " %%%%%%%%%%%%%%%%%%%%%%%%         ***         %%%%%%%%%*",
    "%%%%%%%%%%%%%%%%%%%%%%%*        %@@@@@%       %%%%%%%%%*",
    "%%%%%%%%%%%%%%%%%%%%%*         @@@@@@@@@      %%%%%%%%%*",
    " %%%%%%%%%%%%%%%%%%*          %@@@@@@@@@      %%%%%%%%%",
    "  **************             *@@@@@@@@@*     %%%%%%%%%%",
    "                             %@@@@@@@@%     *%%%%%%%%%*         *%%%%%%%%%%%%%%%%*",
    "                             @@@@@@@@@*     *%%%%%%%%*        *%@@@@@@@@@@@@@@@@@@@@@*",
    "                             @@@@@@@@@*      *%%%%%%*        %@@@@@@@@@@@@@@@@@@@@@@@@",
    "                             %@@@@@@@@%         ***        *@@@@@@@@@@@@@@@@@@@@@@@@@@",
    "                             *@@@@@@@@@*                  *@@@@@@@@@@@@@@@@@@@@@@@@@@@*",
    "                              @@@@@@@@@@*                *@@@@@@@@@@@%%%%%%%%%%%%%%%*",
    "                               @@@@@@@@@@%*            *%@@@@@@@@@@%",
    "                               *@@@@@@@@@@@@%**    **%@@@@@@@@@@@*",
    "                                *@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@",
    "                                  %@@@@@@@@@@@@@@@@@@@@@@@@@@@@%",
    "                                    %@@@@@@@@@@@@@@@@@@@@@@@@%",
    "                                      *%@@@@@@@@@@@@@@@@@@%*",
    "                                         *%%@@@@@@@@@@%%*",
];

pub const TUI_COLS: usize = 74;
pub const TUI_ROWS: usize = 22;

const FRAME_MARGIN_X: f64 = 2.0;
const FRAME_MARGIN_Y: f64 = 1.0;
const SHADES: &[u8] = b" .,-~:;=!*#$@";
const WOBBLE: f64 = 0.24;
const SPEED: f64 = 0.032;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Clone, Debug)]
struct LogoPoint {
    x: f64,
    y: f64,
    z: f64,
    original: char,
    inner: bool,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

#[derive(Clone, Debug)]
struct LogoModel {
    points: Vec<LogoPoint>,
    bounds: Bounds,
}

#[derive(Clone, Copy, Debug)]
struct RenderOptions {
    cols: usize,
    rows: usize,
    wobble: f64,
    use_original_glyphs: bool,
}

pub fn spawn_logo_animation() -> watch::Receiver<String> {
    let model = build_logo_model();
    let options = RenderOptions {
        cols: TUI_COLS,
        rows: TUI_ROWS,
        wobble: WOBBLE,
        use_original_glyphs: false,
    };
    let initial_frame = render_ascii(&model, 0.0, options);
    let (tx, rx) = watch::channel(initial_frame);

    tokio::spawn(async move {
        let mut angle = 0.0;
        let mut interval = tokio::time::interval(FRAME_INTERVAL);

        interval.tick().await;
        loop {
            interval.tick().await;
            angle += SPEED;

            if tx.send(render_ascii(&model, angle, options)).is_err() {
                break;
            }
        }
    });

    rx
}

fn build_logo_model() -> LogoModel {
    let height = LOGO.len() as f64;
    let width = LOGO.iter().map(|line| line.len()).max().unwrap_or(0);
    let cx = width as f64 / 2.0;
    let cy = height / 2.0;
    let mut points = Vec::new();

    for (y, line) in LOGO.iter().enumerate() {
        let line_bytes = line.as_bytes();

        for x in 0..width {
            let ch = line_bytes.get(x).copied().unwrap_or(b' ') as char;
            if ch == ' ' {
                continue;
            }

            let inner = ch == '@';
            let edge = ch == '*';
            let nx = x as f64 - cx;
            let ny = y as f64 - cy;

            let rx = nx / 42.0;
            let ry = ny / 18.0;
            let radial = (rx * rx + ry * ry).sqrt();
            let dome = (1.0 - radial * 0.72).max(0.0);
            let z = (if inner { 0.38 } else { 0.06 }) + dome * 0.34 + if edge { 0.08 } else { 0.0 };

            points.push(LogoPoint {
                x: nx,
                y: ny,
                z,
                original: ch,
                inner,
            });
        }
    }

    let bounds = points.iter().fold(
        Bounds {
            min_x: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
        },
        |bounds, point| Bounds {
            min_x: bounds.min_x.min(point.x),
            max_x: bounds.max_x.max(point.x),
            min_y: bounds.min_y.min(point.y),
            max_y: bounds.max_y.max(point.y),
        },
    );

    LogoModel { points, bounds }
}

fn project_static_silhouette(
    point: &LogoPoint,
    bounds: Bounds,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let drawable_width = cols as f64 - FRAME_MARGIN_X * 2.0 - 1.0;
    let drawable_height = rows as f64 - FRAME_MARGIN_Y * 2.0 - 1.0;
    let logo_width = (bounds.max_x - bounds.min_x).max(1.0);
    let logo_height = (bounds.max_y - bounds.min_y).max(1.0);
    let scale = (drawable_width / logo_width).min(drawable_height / logo_height);

    let rendered_width = logo_width * scale;
    let rendered_height = logo_height * scale;
    let left = ((cols as f64 - rendered_width) / 2.0).floor();
    let top = ((rows as f64 - rendered_height) / 2.0).floor();
    let sx = (left + (point.x - bounds.min_x) * scale).round();
    let sy = (top + (point.y - bounds.min_y) * scale).round();

    (sx.max(0.0) as usize, sy.max(0.0) as usize)
}

fn render_ascii(model: &LogoModel, angle: f64, options: RenderOptions) -> String {
    let mut out = vec![vec![' '; options.cols]; options.rows];
    let mut zbuf = vec![vec![f64::NEG_INFINITY; options.cols]; options.rows];

    let ax = angle * 0.74;
    let ay = angle;
    let az = (angle * 0.55).sin() * options.wobble;

    let cos_x = ax.cos();
    let sin_x = ax.sin();
    let cos_y = ay.cos();
    let sin_y = ay.sin();
    let cos_z = az.cos();
    let sin_z = az.sin();

    for point in &model.points {
        let x = point.x / 42.0;
        let y = point.y / 18.0;
        let z = point.z;

        let y1 = y * cos_x - z * sin_x;
        let z1 = y * sin_x + z * cos_x;
        let x2 = x * cos_y + z1 * sin_y;
        let z2 = -x * sin_y + z1 * cos_y;
        let x3 = x2 * cos_z - y1 * sin_z;

        let (sx, sy) = project_static_silhouette(point, model.bounds, options.cols, options.rows);
        if sx >= options.cols || sy >= options.rows || z2 <= zbuf[sy][sx] {
            continue;
        }

        zbuf[sy][sx] = z2;

        let sweep = (angle * 1.2 + x3 * 5.2 + y1 * 3.4).sin();
        let light =
            (0.48 + z2 * 0.34 + sweep * 0.2 + if point.inner { 0.08 } else { 0.0 }).clamp(0.0, 1.0);
        let idx = (light * (SHADES.len() - 1) as f64).round() as usize;
        out[sy][sx] = if options.use_original_glyphs {
            point.original
        } else {
            SHADES[idx] as char
        };
    }

    out.into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_logo_model_returns_visible_finite_points() {
        let model = build_logo_model();

        assert!(!model.points.is_empty());
        assert!(model.points.iter().any(|point| point.original == '@'));
        assert!(model.points.iter().any(|point| point.original == '%'));
        assert!(
            model
                .points
                .iter()
                .all(|point| point.x.is_finite() && point.y.is_finite() && point.z.is_finite())
        );
        assert!(model.bounds.max_x > model.bounds.min_x);
        assert!(model.bounds.max_y > model.bounds.min_y);
    }

    #[test]
    fn render_ascii_returns_fixed_size_shaded_frame() {
        let model = build_logo_model();
        let frame = render_ascii(
            &model,
            0.0,
            RenderOptions {
                cols: TUI_COLS,
                rows: TUI_ROWS,
                wobble: WOBBLE,
                use_original_glyphs: false,
            },
        );
        let lines = frame.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), TUI_ROWS);
        assert!(lines.iter().all(|line| line.len() == TUI_COLS));
        assert!(frame.contains('\n'));
        assert!(frame.chars().any(|ch| ".,-~:;=!*#$@".contains(ch)));
    }

    #[test]
    fn render_ascii_can_use_original_logo_glyphs() {
        let model = build_logo_model();
        let frame = render_ascii(
            &model,
            0.5,
            RenderOptions {
                cols: TUI_COLS,
                rows: TUI_ROWS,
                wobble: WOBBLE,
                use_original_glyphs: true,
            },
        );

        assert!(frame.chars().any(|ch| "%@*".contains(ch)));
    }

    #[test]
    fn projection_keeps_points_inside_compact_tui_frame() {
        let model = build_logo_model();

        for point in &model.points {
            let (sx, sy) = project_static_silhouette(point, model.bounds, TUI_COLS, TUI_ROWS);

            assert!(sx < TUI_COLS);
            assert!(sy < TUI_ROWS);
        }
    }
}
