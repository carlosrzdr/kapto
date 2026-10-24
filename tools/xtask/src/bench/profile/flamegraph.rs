// SPDX-License-Identifier: Apache-2.0

//! A flamegraph of a profile's stacks, as an SVG a browser opens: one box
//! per function, as wide as its share of the samples, on top of what called
//! it, each thread a column of its own over the whole. Hovering a box gives
//! its name and samples.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The drawing's width, in pixels.
const WIDTH: f64 = 1200.0;
/// Each box's height.
const FRAME: f64 = 16.0;
/// Room above the boxes for the title.
const TOP: f64 = 32.0;
/// Boxes narrower than this aren't drawn: too thin to see or hover.
const NARROWEST: f64 = 0.3;
/// An average character's width at the boxes' font size, to fit names.
const CHARACTER: f64 = 6.6;

/// A function and what it called, by name, with its samples.
#[derive(Debug, Default)]
struct Node {
    samples: u64,
    called: BTreeMap<String, Self>,
}

impl Node {
    fn add<'a>(&mut self, path: impl IntoIterator<Item = &'a str>, samples: u64) {
        self.samples += samples;
        let mut node = self;
        for name in path {
            node = node.called.entry(name.to_owned()).or_default();
            node.samples += samples;
        }
    }

    fn depth(&self) -> usize {
        self.called
            .values()
            .map(|node| node.depth() + 1)
            .max()
            .unwrap_or(0)
    }
}

/// The flamegraph of `stacks`, each its frames from the outermost, its
/// thread's name first, and its samples.
pub(super) fn svg<'a>(
    title: &str,
    stacks: impl IntoIterator<Item = (Vec<&'a str>, u64)>,
) -> String {
    let mut all = Node::default();
    for (path, samples) in stacks {
        all.add(path, samples);
    }
    let height = TOP + (all.depth() + 1) as f64 * FRAME + 8.0;
    let mut text = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{WIDTH}\" height=\"{height}\" \
         viewBox=\"0 0 {WIDTH} {height}\" font-family=\"monospace\" font-size=\"11\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"#fdfaf3\"/>\n\
         <text x=\"{}\" y=\"20\" text-anchor=\"middle\" font-size=\"15\">{}</text>\n",
        WIDTH / 2.0,
        escaped(title)
    );
    if all.samples > 0 {
        let scale = WIDTH / all.samples as f64;
        draw(
            &mut text,
            "all",
            &all,
            (0.0, height - 8.0 - FRAME),
            scale,
            all.samples,
        );
    }
    text.push_str("</svg>\n");
    text
}

/// Draws `node`'s box with its bottom-left corner at `at`, then what it
/// called above it, left to right.
fn draw(text: &mut String, name: &str, node: &Node, at: (f64, f64), scale: f64, all: u64) {
    let width = node.samples as f64 * scale;
    if width < NARROWEST {
        return;
    }
    let (x, y) = at;
    let share = 100.0 * node.samples as f64 / all as f64;
    let _ = writeln!(
        text,
        "<g><title>{} ({} samples, {share:.2} %)</title>\
         <rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{:.2}\" height=\"{:.0}\" fill=\"{}\" rx=\"2\"/>",
        escaped(name),
        node.samples,
        (width - 0.5).max(0.1),
        FRAME - 1.0,
        colour(name)
    );
    let fits = |characters: usize| characters as f64 * CHARACTER <= width - 6.0;
    if fits(3) {
        let shown: String = if fits(name.chars().count()) {
            name.to_owned()
        } else {
            let count = name.chars().count();
            let kept = (1..count)
                .take_while(|&kept| fits(kept + 2))
                .last()
                .unwrap_or(1);
            name.chars().take(kept).chain("..".chars()).collect()
        };
        let _ = writeln!(
            text,
            "<text x=\"{:.2}\" y=\"{:.2}\">{}</text>",
            x + 3.0,
            y + FRAME - 4.5,
            escaped(&shown)
        );
    }
    text.push_str("</g>\n");
    let mut left = x;
    for (called, child) in &node.called {
        draw(text, called, child, (left, y - FRAME), scale, all);
        left += child.samples as f64 * scale;
    }
}

/// A warm colour, the same for the same name: the kernel's and libraries'
/// boxes greyer than code built from source.
fn colour(name: &str) -> String {
    let hash = name.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    let vary = |range: u32, shift: u32| (hash >> shift) % range;
    if name.starts_with('[') {
        format!("rgb({0},{0},{1})", 170 + vary(40, 0), 160 + vary(40, 8))
    } else {
        format!(
            "rgb({},{},{})",
            205 + vary(50, 0),
            80 + vary(130, 8),
            30 + vary(40, 16)
        )
    }
}

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths(svg: &str) -> Vec<(String, f64)> {
        svg.lines()
            .filter_map(|line| {
                let name = line.split("<title>").nth(1)?.split(" (").next()?;
                let width = line.split("width=\"").nth(1)?.split('"').next()?;
                Some((name.to_owned(), width.parse().ok()?))
            })
            .collect()
    }

    #[test]
    fn each_function_is_as_wide_as_its_share_of_the_samples_on_what_called_it() {
        let drawn = svg(
            "reference",
            [
                (vec!["cap-writer", "run", "write"], 2),
                (vec!["cap-writer", "run"], 1),
                (vec!["cap-rx", "receive"], 1),
            ],
        );
        let widths = widths(&drawn);
        let width = |name: &str| widths.iter().find(|(n, _)| n == name).map(|(_, w)| *w);
        assert_eq!(width("all"), Some(WIDTH - 0.5));
        assert_eq!(width("cap-writer"), Some(WIDTH * 0.75 - 0.5));
        assert_eq!(width("write"), Some(WIDTH * 0.5 - 0.5));
        assert_eq!(width("receive"), Some(WIDTH * 0.25 - 0.5));
        assert!(
            drawn.contains("<title>run (3 samples, 75.00 %)</title>"),
            "{drawn}"
        );
        // What a function called sits on top of it.
        let y = |name: &str| {
            let line = drawn
                .lines()
                .find(|line| line.contains(&format!("<title>{name} ")))?;
            line.split("y=\"")
                .nth(1)?
                .split('"')
                .next()?
                .parse::<f64>()
                .ok()
        };
        assert_eq!(
            y("run").zip(y("write")).map(|(run, write)| run - write),
            Some(FRAME)
        );
    }

    #[test]
    fn names_are_escaped_and_boxes_too_thin_to_see_left_out() {
        let mut stacks = vec![(
            vec!["cap-rx", "<UdpTransport as Transport>::receive"],
            10_000,
        )];
        stacks.push((vec!["cap-rx", "rare"], 1));
        let drawn = svg("a & b", stacks);
        assert!(
            drawn.contains("&lt;UdpTransport as Transport&gt;::receive"),
            "{drawn}"
        );
        assert!(drawn.contains(">a &amp; b</text>"), "{drawn}");
        assert!(!drawn.contains("<title>rare"), "{drawn}");
    }

    #[test]
    fn no_samples_draw_only_the_title() {
        let drawn = svg("empty", []);
        assert!(drawn.contains(">empty</text>"));
        assert!(!drawn.contains("<rect x="));
    }
}
