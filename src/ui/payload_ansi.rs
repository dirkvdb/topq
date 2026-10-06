//! Decode ANSI SGR attributes without interpreting terminal control commands.

use std::ops::Range;

use anstyle_parse::{Params, Parser, Perform};
use gpui_kit::component::Theme;
use gpui_kit::{FontStyle, FontWeight, HighlightStyle, Hsla, StrikethroughStyle, UnderlineStyle, px, rgb};

/// Returns decoded text and sorted, non-overlapping UTF-8 byte highlights.
///
/// `anstyle_parse` owns escape syntax and malformed-sequence recovery. The
/// adapter applies semicolon-separated SGR attributes, preserves printable
/// Unicode and newline/tab/carriage return, and ignores other terminal commands
/// (including cursor movement, OSC titles and links). Incomplete escapes are
/// discarded. Invalid extended colors leave the active attributes unchanged;
/// unknown SGR attributes are ignored. Unstyled text has no highlight entry.
pub(super) fn parse(text: &str, theme: &Theme) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut output = PayloadPerformer {
        decoded: String::with_capacity(text.len()),
        highlights: Vec::new(),
        style: HighlightStyle::default(),
        theme,
    };
    let mut parser: Parser = Parser::default();
    for byte in text.bytes() {
        parser.advance(&mut output, byte);
    }
    (output.decoded, output.highlights)
}

struct PayloadPerformer<'a> {
    decoded: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    style: HighlightStyle,
    theme: &'a Theme,
}

impl Perform for PayloadPerformer<'_> {
    fn print(&mut self, character: char) {
        // The parser delivers DEL through print rather than execute.
        if character == '\x7f' {
            return;
        }
        let mut buffer = [0; 4];
        append(
            character.encode_utf8(&mut buffer),
            self.style,
            &mut self.decoded,
            &mut self.highlights,
        );
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | b'\t' | b'\r') {
            self.print(char::from(byte));
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        if action == b'm'
            && intermediates.is_empty()
            && !ignore
            && let Some(style) = sgr(params, self.style, self.theme)
        {
            self.style = style;
        }
    }

    // All other Perform callbacks intentionally retain their no-op defaults:
    // payloads must not move cursors, change window titles, or activate links.
}

fn append(text: &str, style: HighlightStyle, decoded: &mut String, highlights: &mut Vec<(Range<usize>, HighlightStyle)>) {
    let start = decoded.len();
    decoded.push_str(text);
    if text.is_empty() || style == HighlightStyle::default() {
        return;
    }
    if let Some((range, previous_style)) = highlights.last_mut()
        && range.end == start
        && *previous_style == style
    {
        range.end = decoded.len();
    } else {
        highlights.push((start..decoded.len(), style));
    }
}

fn sgr(params: &Params, mut style: HighlightStyle, theme: &Theme) -> Option<HighlightStyle> {
    if params.is_empty() {
        return Some(HighlightStyle::default());
    }
    let mut parameters = params.iter();
    while let Some(parameter) = parameters.next() {
        // Colon subparameters are parsed by the library, but are not mapped here.
        let [code] = parameter else { continue };
        match *code {
            0 => style = HighlightStyle::default(),
            1 => style.font_weight = Some(FontWeight::BOLD),
            3 => style.font_style = Some(FontStyle::Italic),
            4 => {
                style.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    ..Default::default()
                });
            }
            9 => {
                style.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.),
                    ..Default::default()
                });
            }
            22 => style.font_weight = None,
            23 => style.font_style = None,
            24 => style.underline = None,
            29 => style.strikethrough = None,
            30..=37 => style.color = Some(indexed_color((code - 30) as u8, theme)),
            40..=47 => style.background_color = Some(indexed_color((code - 40) as u8, theme)),
            90..=97 => style.color = Some(indexed_color((code - 90 + 8) as u8, theme)),
            100..=107 => style.background_color = Some(indexed_color((code - 100 + 8) as u8, theme)),
            39 => style.color = None,
            49 => style.background_color = None,
            38 | 48 => {
                let color = match parameters.next()? {
                    [5] => indexed_color(color_component(parameters.next()?)?, theme),
                    [2] => {
                        let red = color_component(parameters.next()?)?;
                        let green = color_component(parameters.next()?)?;
                        let blue = color_component(parameters.next()?)?;
                        rgb((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)).into()
                    }
                    _ => return None,
                };
                if *code == 38 {
                    style.color = Some(color);
                } else {
                    style.background_color = Some(color);
                }
            }
            _ => {}
        }
    }
    Some(style)
}

fn color_component(parameter: &[u16]) -> Option<u8> {
    let [value] = parameter else { return None };
    u8::try_from(*value).ok()
}

fn indexed_color(index: u8, theme: &Theme) -> Hsla {
    // Named ANSI colors are terminal-dependent. Use theme roles for both normal
    // and bright variants; the theme has no separate bright ANSI tokens.
    if index < 16 {
        return match index % 8 {
            0 => theme.muted_foreground,
            1 => theme.danger,
            2 => theme.success,
            3 => theme.warning,
            4 => theme.link,
            5 => theme.primary,
            6 => theme.info,
            _ => theme.foreground,
        };
    }
    // Extended colors and explicit RGB are payload data, not UI theme colors.
    let color = match index {
        16..=231 => {
            const LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
            let cube = usize::from(index - 16);
            (LEVELS[cube / 36] << 16) | (LEVELS[cube / 6 % 6] << 8) | LEVELS[cube % 6]
        }
        _ => {
            let gray = 8 + 10 * u32::from(index - 232);
            (gray << 16) | (gray << 8) | gray
        }
    };
    rgb(color).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
        super::parse(text, &Theme::default())
    }

    fn indexed_color(index: u8) -> Hsla {
        super::indexed_color(index, &Theme::default())
    }

    #[test]
    fn screenshot_log_has_bold_red_highlight() {
        let message = "[E][api:128]: No clients; rebooting";
        assert_eq!(
            parse(&format!("\x1b[1;31m{message}\x1b[0m")),
            (
                message.into(),
                vec![(
                    0..message.len(),
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        color: Some(Theme::default().danger),
                        ..Default::default()
                    }
                )]
            )
        );
    }

    #[test]
    fn plain_text_and_literal_escape_notation_are_unchanged() {
        for text in ["", "plain\ntext", "世界🙂", r"\x1b[31mred\x1b[0m", "[31mred", "\u{009b}31mred"] {
            assert_eq!(parse(text), (text.into(), vec![]));
        }
    }

    #[test]
    fn unicode_ranges_use_decoded_byte_offsets() {
        let (text, highlights) = parse("é\x1b[3m世界🙂\x1b[0m!");
        assert_eq!(text, "é世界🙂!");
        assert_eq!(
            highlights,
            vec![(
                2..12,
                HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                }
            )]
        );
        assert_eq!(&text[highlights[0].0.clone()], "世界🙂");
    }

    #[test]
    fn individual_attribute_resets_preserve_other_attributes() {
        let (_, highlights) = parse("\x1b[1;3;4;9mA\x1b[22mB\x1b[23mC\x1b[24mD\x1b[29mE");
        assert_eq!(highlights.len(), 4);
        let mut expected = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            font_style: Some(FontStyle::Italic),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(highlights[0], (0..1, expected));
        expected.font_weight = None;
        assert_eq!(highlights[1], (1..2, expected));
        expected.font_style = None;
        assert_eq!(highlights[2], (2..3, expected));
        expected.underline = None;
        assert_eq!(highlights[3], (3..4, expected));
    }

    #[test]
    fn full_and_empty_resets_clear_all_attributes() {
        for reset in ["\x1b[0m", "\x1b[m", "\x1b[;m"] {
            let (text, highlights) = parse(&format!("\x1b[1;3;4;9;31;42mA{reset}B"));
            assert_eq!(text, "AB");
            assert_eq!(highlights.len(), 1);
            assert_eq!(highlights[0].0, 0..1);
        }
    }

    #[test]
    fn standard_and_bright_foreground_and_background_match_theme() {
        for index in 0..16_u8 {
            let foreground = if index < 8 { 30 + index } else { 90 + index - 8 };
            let background = if index < 8 { 40 + index } else { 100 + index - 8 };
            assert_eq!(
                parse(&format!("\x1b[{foreground};{background}mX")),
                (
                    "X".into(),
                    vec![(
                        0..1,
                        HighlightStyle {
                            color: Some(indexed_color(index)),
                            background_color: Some(indexed_color(index)),
                            ..Default::default()
                        }
                    )]
                )
            );
        }
    }

    #[test]
    fn named_and_base_indexed_colors_follow_custom_theme_tokens() {
        let mut theme = Theme::default();
        theme.danger = theme.primary;
        theme.success = theme.info;
        let colors = [
            theme.muted_foreground,
            theme.danger,
            theme.success,
            theme.warning,
            theme.link,
            theme.primary,
            theme.info,
            theme.foreground,
        ];
        for index in 0..16_u8 {
            let code = if index < 8 { 30 + index } else { 90 + index - 8 };
            for text in [format!("\x1b[{code}mX"), format!("\x1b[38;5;{index}mX")] {
                let (_, highlights) = super::parse(&text, &theme);
                assert_eq!(highlights[0].1.color, Some(colors[usize::from(index % 8)]));
            }
        }
    }

    #[test]
    fn default_color_resets_are_independent() {
        let (_, highlights) = parse("\x1b[31;44mA\x1b[39mB\x1b[49mC");
        assert_eq!(
            highlights,
            vec![
                (
                    0..1,
                    HighlightStyle {
                        color: Some(Theme::default().danger),
                        background_color: Some(Theme::default().link),
                        ..Default::default()
                    }
                ),
                (
                    1..2,
                    HighlightStyle {
                        background_color: Some(Theme::default().link),
                        ..Default::default()
                    }
                ),
            ]
        );
    }

    #[test]
    fn indexed_colors_cover_base_cube_and_grayscale_boundaries() {
        for (index, hex) in [(16, 0x000000), (67, 0x5f87af), (231, 0xffffff), (232, 0x080808), (255, 0xeeeeee)] {
            assert_eq!(
                parse(&format!("\x1b[38;5;{index};48;5;{index}mX")),
                (
                    "X".into(),
                    vec![(
                        0..1,
                        HighlightStyle {
                            color: Some(rgb(hex).into()),
                            background_color: Some(rgb(hex).into()),
                            ..Default::default()
                        }
                    )]
                )
            );
        }
    }

    #[test]
    fn truecolor_supports_foreground_background_and_following_attributes() {
        assert_eq!(
            parse("\x1b[38;2;1;128;255;48;2;255;0;16;1mX"),
            (
                "X".into(),
                vec![(
                    0..1,
                    HighlightStyle {
                        color: Some(rgb(0x0180ff).into()),
                        background_color: Some(rgb(0xff0010).into()),
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    }
                )]
            )
        );
    }

    #[test]
    fn incomplete_and_malformed_sequences_follow_library_recovery() {
        for (input, expected) in [
            ("\x1b", ""),
            ("\x1b[", ""),
            ("\x1b[31", ""),
            ("\x1b[31visible message", "isible message"),
            ("\x1b[31 hello m", "ello m"),
            ("\x1b[999999999999999999999mX", "X"),
        ] {
            assert_eq!(parse(input), (expected.into(), vec![]), "{input:?}");
        }
    }

    #[test]
    fn invalid_extended_colors_are_ignored() {
        for input in [
            "\x1b[38mX",
            "\x1b[38;5mX",
            "\x1b[38;5;256mX",
            "\x1b[48;2;255;0mX",
            "\x1b[38;2;256;0;0mX",
            "\x1b[38;7;1mX",
        ] {
            assert_eq!(parse(input), ("X".into(), vec![]), "{input:?}");
        }
    }

    #[test]
    fn terminal_commands_are_ignored_without_rewriting_text() {
        for command in [
            "\x1b[2J",
            "\x1b[10;20H",
            "\x1b[2D",
            "\x1b[?25m",
            "\x1b[31:1m",
            "\x1b[1 m",
            "\x1b]0;title\x07",
            "\x1b]8;;https://example.com\x1b\\",
            "\x1b]8;;\x1b\\",
            "\x1bPignored device command\x1b\\",
            "\x1b7",
            "\x1b8",
            "\x07",
            "\x08",
            "\x00",
            "\x7f",
        ] {
            assert_eq!(parse(&format!("A{command}B")), ("AB".into(), vec![]), "{command:?}");
        }
    }

    #[test]
    fn newline_tab_and_carriage_return_preserve_text_and_style() {
        let content = "世界\n\t\r🙂";
        assert_eq!(
            parse(&format!("\x1b[1m{content}\x1b[0m")),
            (
                content.into(),
                vec![(
                    0..content.len(),
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    }
                )]
            )
        );
    }

    #[test]
    fn malformed_sgr_does_not_partially_change_style() {
        let text = "A\x1b[22;48;2;1;2mB";
        assert_eq!(
            parse(&format!("\x1b[1m{text}")),
            (
                "AB".into(),
                vec![(
                    0..2,
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    }
                )]
            )
        );
    }

    #[test]
    fn valid_sequence_after_malformed_prefix_is_still_decoded() {
        assert_eq!(
            parse("\x1b[31broken\x1b[3m世界"),
            (
                "roken世界".into(),
                vec![(
                    5..11,
                    HighlightStyle {
                        font_style: Some(FontStyle::Italic),
                        ..Default::default()
                    }
                )]
            )
        );
    }

    #[test]
    fn unknown_numeric_codes_are_stripped_and_identical_runs_merge() {
        assert_eq!(
            parse("\x1b[999mplain\x1b[1mA\x1b[777;1mB\x1b[0mC"),
            (
                "plainABC".into(),
                vec![(
                    5..7,
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    }
                )]
            )
        );
    }

    #[test]
    fn sequences_without_visible_text_emit_no_empty_ranges() {
        assert_eq!(parse("\x1b[1m\x1b[31m\x1b[0m"), (String::new(), vec![]));
    }
}
