//! Metadata derivation and the CSV the workflow consumes.
//!
//! A sheet is only half the deliverable: the other half is the manifest that
//! says what each cell *is*. This module derives that metadata — a readable
//! name from a pattern, a slug, tags from the measured geometry — and writes it
//! as RFC 4180 CSV, with the reader that proves the round trip.
//!
//! Derivation is deliberately boring and deterministic. The name comes from a
//! pattern the user writes (`{sheet}-{index:03}`), the slug from the name, and
//! the tags from numbers the leveling already computed — so the same library
//! exports the same manifest every time, whatever order the icons were read in.

use super::{fmt_num, ExportError};
use crate::sheet::{IconMetrics, IconPlacement};

/// The delimiter a plain "comma separated values" file uses.
pub const DEFAULT_DELIMITER: u8 = b',';

/// One column of the manifest — the vocabulary the CSV wizard maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    /// 1-based position on the sheet.
    Index,
    /// Human-readable name (from the pattern).
    Name,
    /// Filesystem-safe form of the name.
    Slug,
    /// Space-separated tags derived from the measured geometry.
    Tags,
    /// The icon's own file inside the export (e.g. `svg/icon-001.svg`).
    File,
    /// 1-based grid row on the sheet.
    Row,
    /// 1-based grid column on the sheet.
    Col,
    /// Source crop width in pixels.
    Width,
    /// Source crop height in pixels.
    Height,
    /// Placed ink size on the sheet, in pixels (the leveling's own number).
    Ink,
    /// Stroke weight in pixels (`2 × max inscribed radius`).
    Stroke,
    /// Ink area over convex-hull area.
    Solidity,
    /// Ink pixel count in the source crop.
    Area,
    /// Trace preset the icon was vectorized with.
    Preset,
    /// Distinct colours in the traced palette.
    Colours,
    /// The icon's id in the project (hex).
    Id,
}

impl Column {
    /// Every column, in the order the wizard offers them.
    pub const ALL: [Self; 16] = [
        Self::Index,
        Self::Name,
        Self::Slug,
        Self::Tags,
        Self::File,
        Self::Row,
        Self::Col,
        Self::Width,
        Self::Height,
        Self::Ink,
        Self::Stroke,
        Self::Solidity,
        Self::Area,
        Self::Preset,
        Self::Colours,
        Self::Id,
    ];

    /// The header this column writes.
    #[must_use]
    pub const fn header(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Name => "name",
            Self::Slug => "slug",
            Self::Tags => "tags",
            Self::File => "file",
            Self::Row => "row",
            Self::Col => "col",
            Self::Width => "width",
            Self::Height => "height",
            Self::Ink => "ink",
            Self::Stroke => "stroke",
            Self::Solidity => "solidity",
            Self::Area => "area",
            Self::Preset => "preset",
            Self::Colours => "colours",
            Self::Id => "id",
        }
    }

    /// Parses a header (case-insensitive); anything unknown is `None`, so a
    /// wizard can tell the user which column it did not understand.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|c| c.header().eq_ignore_ascii_case(name))
    }
}

/// The columns a fresh export uses: what a designer needs to place the icons,
/// without the diagnostics.
pub const CSV_COLUMNS: [Column; 9] = [
    Column::Index,
    Column::Name,
    Column::Slug,
    Column::Tags,
    Column::File,
    Column::Row,
    Column::Col,
    Column::Width,
    Column::Height,
];

/// What the caller knows about one icon that the geometry does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconMeta {
    /// The plan's id for this icon.
    pub id: u32,
    /// The icon's id in the project, as hex.
    pub id_hex: String,
    /// The source sheet's file stem (`icons-01`), used by `{sheet}`.
    pub sheet_stem: String,
    /// The preset the icon was traced with.
    pub preset: String,
    /// Distinct colours in the traced palette.
    pub colours: u32,
}

/// One row of the manifest: every column's value, already formatted.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetRow {
    /// 1-based position on the sheet.
    pub index: u32,
    /// Name, after the pattern was expanded.
    pub name: String,
    /// Slug derived from [`SheetRow::name`].
    pub slug: String,
    /// Tags derived from the geometry.
    pub tags: String,
    /// The icon's file inside the export.
    pub file: String,
    /// 1-based grid row.
    pub row: u32,
    /// 1-based grid column.
    pub col: u32,
    /// Source crop width.
    pub width: u32,
    /// Source crop height.
    pub height: u32,
    /// Placed ink size in sheet pixels.
    pub ink: f32,
    /// Stroke weight in pixels.
    pub stroke: f32,
    /// Solidity in `(0, 1]`.
    pub solidity: f32,
    /// Ink pixel count.
    pub area: u32,
    /// Trace preset.
    pub preset: String,
    /// Distinct palette colours.
    pub colours: u32,
    /// Project id, hex.
    pub id: String,
}

impl SheetRow {
    /// One column's value as text.
    #[must_use]
    pub fn field(&self, column: Column) -> String {
        match column {
            Column::Index => self.index.to_string(),
            Column::Name => self.name.clone(),
            Column::Slug => self.slug.clone(),
            Column::Tags => self.tags.clone(),
            Column::File => self.file.clone(),
            Column::Row => self.row.to_string(),
            Column::Col => self.col.to_string(),
            Column::Width => self.width.to_string(),
            Column::Height => self.height.to_string(),
            Column::Ink => fmt_num(self.ink),
            Column::Stroke => fmt_num(self.stroke),
            Column::Solidity => fmt_num(self.solidity),
            Column::Area => self.area.to_string(),
            Column::Preset => self.preset.clone(),
            Column::Colours => self.colours.to_string(),
            Column::Id => self.id.clone(),
        }
    }
}

/// Options the CSV wizard sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsvOptions {
    /// Field delimiter (`,` or `;` or a tab).
    pub delimiter: u8,
    /// Write the header row.
    pub header: bool,
    /// Name pattern — see [`expand_pattern`].
    pub name_pattern: String,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: DEFAULT_DELIMITER,
            header: true,
            name_pattern: "{sheet}-{index:03}".to_string(),
        }
    }
}

/// Which icons of the sheet the caller is deriving rows for.
///
/// Kept separate from [`crate::sheet::IconInput`] on purpose: the plan needs
/// geometry, the manifest needs identity, and neither should have to carry the
/// other's fields.
#[must_use]
pub fn derive_row(
    meta: &IconMeta,
    placement: &IconPlacement,
    metrics: &IconMetrics,
    columns: u32,
    file: &str,
    options: &CsvOptions,
) -> SheetRow {
    let (row, col) = grid_position(placement, columns);
    let name = expand_pattern(
        &options.name_pattern,
        &PatternValues {
            sheet: &meta.sheet_stem,
            index: placement.id,
            row,
            col,
            preset: &meta.preset,
        },
    );
    SheetRow {
        index: placement.id,
        slug: slugify(&name),
        name,
        tags: derive_tags(meta.preset.as_str(), metrics),
        file: file.to_string(),
        row,
        col,
        width: metrics.ink_w.round().max(1.0) as u32,
        height: metrics.ink_h.round().max(1.0) as u32,
        ink: placement.scale * metrics.ink_long_side(),
        stroke: metrics.stroke,
        solidity: metrics.solidity,
        area: metrics.ink_area,
        preset: meta.preset.clone(),
        colours: meta.colours,
        id: meta.id_hex.clone(),
    }
}

/// 1-based `(row, col)` of a placement on a grid of `columns` cells per row.
///
/// The plan stores a cell's pixel origin; the grid position is derived from the
/// index it was placed at, which is the icon's 1-based position in reading
/// order.
#[must_use]
pub fn grid_position(placement: &IconPlacement, columns: u32) -> (u32, u32) {
    let columns = columns.max(1);
    let index = placement.id.saturating_sub(1);
    (index / columns + 1, index % columns + 1)
}

/// Values a name pattern can interpolate.
#[derive(Clone, Copy, Debug)]
pub struct PatternValues<'a> {
    /// The source sheet's file stem.
    pub sheet: &'a str,
    /// 1-based index on the sheet.
    pub index: u32,
    /// 1-based row.
    pub row: u32,
    /// 1-based column.
    pub col: u32,
    /// The trace preset.
    pub preset: &'a str,
}

/// Expands a name pattern.
///
/// Tokens: `{sheet}`, `{index}`, `{row}`, `{col}`, `{preset}`. A token may ask
/// for zero padding with `:N` — `{index:03}` gives `007`. An unknown token is
/// left as written, so a typo shows up in the first row instead of disappearing.
#[must_use]
pub fn expand_pattern(pattern: &str, values: &PatternValues<'_>) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // An unclosed brace is literal text.
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &after[..close];
        let (name, pad) = match token.split_once(':') {
            Some((name, pad)) => (name, pad.parse::<usize>().ok()),
            None => (token, None),
        };
        match name {
            "sheet" => out.push_str(values.sheet),
            "preset" => out.push_str(values.preset),
            "index" => out.push_str(&pad_number(values.index, pad)),
            "row" => out.push_str(&pad_number(values.row, pad)),
            "col" => out.push_str(&pad_number(values.col, pad)),
            _ => {
                out.push('{');
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

fn pad_number(value: u32, pad: Option<usize>) -> String {
    match pad {
        Some(width) if width > 0 => format!("{value:0width$}"),
        _ => value.to_string(),
    }
}

/// A filesystem- and URL-safe form of a name: lowercase ASCII letters, digits
/// and single dashes, with everything else dropped.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else if c.is_alphanumeric() {
            // Non-ASCII letters are still meaningful; keep them, lowercased.
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            for lower in c.to_lowercase() {
                out.push(lower);
            }
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "icon".to_string()
    } else {
        out
    }
}

/// Tags derived from the measured geometry and the preset.
///
/// The vocabulary is small and fixed, because the point of a tag is that a
/// designer can filter by it: how solid the mark is, how thick its stroke is
/// next to the preset, and whether it is monochrome. Anything cleverer belongs
/// in the review system (§3.6), not in a manifest.
#[must_use]
pub fn derive_tags(preset: &str, metrics: &IconMetrics) -> String {
    let mut tags: Vec<&str> = Vec::with_capacity(4);
    if metrics.solidity >= 0.85 {
        tags.push("solid");
    } else if metrics.solidity <= 0.60 {
        tags.push("hollow");
    } else {
        tags.push("mixed");
    }
    // "thin"/"thick" are relative to the icon's own box: a 4 px stroke inside a
    // 16 px icon is thick, the same stroke inside a 200 px icon is thin.
    let long_side = metrics.ink_long_side().max(1.0);
    let relative = metrics.stroke / long_side;
    if relative <= 0.15 {
        tags.push("thin");
    } else if relative >= 0.30 {
        tags.push("thick");
    } else {
        tags.push("even");
    }
    tags.push(if metrics.ink_area == 0 {
        "empty"
    } else {
        "ink"
    });
    tags.push(preset);
    tags.join(" ")
}

/// A CSV problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CsvError {
    /// A quoted field was never closed (a truncated file).
    UnterminatedQuote {
        /// Byte offset where the field started.
        at: usize,
    },
    /// The delimiter is not a single printable byte.
    BadDelimiter,
}

impl std::fmt::Display for CsvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnterminatedQuote { at } => {
                write!(f, "quoted field starting at byte {at} never ends")
            }
            Self::BadDelimiter => f.write_str("delimiter must be one ASCII byte"),
        }
    }
}

impl std::error::Error for CsvError {}

/// Writes rows as RFC 4180 CSV (`\r\n` line endings, minimal quoting).
///
/// # Errors
///
/// [`CsvError::BadDelimiter`] for a delimiter that is not a single ASCII byte
/// (a multi-byte separator would make the file unparseable by every reader).
pub fn write_csv(
    rows: &[SheetRow],
    columns: &[Column],
    options: &CsvOptions,
) -> Result<String, CsvError> {
    if !options.delimiter.is_ascii()
        || options.delimiter == b'"'
        || options.delimiter == b'\n'
        || options.delimiter == b'\r'
    {
        return Err(CsvError::BadDelimiter);
    }
    let mut out = String::with_capacity(rows.len() * columns.len() * 12 + 64);
    if options.header {
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                out.push(options.delimiter as char);
            }
            out.push_str(column.header());
        }
        out.push_str("\r\n");
    }
    for row in rows {
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                out.push(options.delimiter as char);
            }
            push_field(&mut out, &row.field(*column), options.delimiter);
        }
        out.push_str("\r\n");
    }
    Ok(out)
}

/// Writes one field, quoting only when RFC 4180 requires it.
fn push_field(out: &mut String, field: &str, delimiter: u8) {
    let needs_quotes = field.contains(delimiter as char)
        || field.contains('"')
        || field.contains('\n')
        || field.contains('\r')
        || field.starts_with(' ')
        || field.ends_with(' ');
    if !needs_quotes {
        out.push_str(field);
        return;
    }
    out.push('"');
    for c in field.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

/// Parses CSV text back into rows of fields (the writer's inverse).
///
/// Accepts `\r\n`, `\n` and `\r` line endings, doubled quotes inside quoted
/// fields, and a trailing newline. A final empty line is not a row.
///
/// # Errors
///
/// [`CsvError::UnterminatedQuote`] when a quoted field runs to the end of the
/// text, and [`CsvError::BadDelimiter`] for a non-ASCII delimiter.
pub fn parse_csv(text: &str, delimiter: u8) -> Result<Vec<Vec<String>>, CsvError> {
    if !delimiter.is_ascii() || delimiter == b'"' {
        return Err(CsvError::BadDelimiter);
    }
    let bytes = text.as_bytes();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut field_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if quoted {
            if b == b'"' {
                if bytes.get(i + 1) == Some(&b'"') {
                    field.push('"');
                    i += 2;
                    continue;
                }
                quoted = false;
                i += 1;
                continue;
            }
            field.push(b as char);
            i += 1;
            continue;
        }
        match b {
            b'"' if field.is_empty() => {
                quoted = true;
                field_start = i;
                i += 1;
            }
            _ if b == delimiter => {
                row.push(std::mem::take(&mut field));
                i += 1;
            }
            b'\r' | b'\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                // CRLF is one line break.
                i += if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            }
            _ => {
                field.push(b as char);
                i += 1;
            }
        }
    }
    if quoted {
        return Err(CsvError::UnterminatedQuote { at: field_start });
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// The file name an icon's own SVG gets inside an export.
#[must_use]
pub fn artwork_file(slug: &str) -> String {
    format!("svg/{slug}.svg")
}

/// Checks the plan's icon count against the art work's, so an export never
/// silently mixes two sheets.
///
/// # Errors
///
/// [`ExportError::MissingArtwork`] for the first id that has no art.
pub fn check_artwork_covers(
    placements: &[IconPlacement],
    artwork: &[super::Artwork],
) -> Result<(), ExportError> {
    for placement in placements {
        if !artwork.iter().any(|a| a.id == placement.id) {
            return Err(ExportError::MissingArtwork { id: placement.id });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::export::Artwork;
    use crate::sheet::Rect;

    fn metrics(side: f32, stroke: f32, solidity: f32) -> IconMetrics {
        IconMetrics {
            ink_x: 0.0,
            ink_y: 0.0,
            ink_w: side,
            ink_h: side,
            ink_area: (side * side * solidity) as u32,
            centroid_x: side * 0.5,
            centroid_y: side * 0.5,
            stroke,
            solidity,
        }
    }

    fn placement(id: u32) -> IconPlacement {
        IconPlacement {
            id,
            cell: (0, 0),
            ink: Rect {
                x: 0.0,
                y: 0.0,
                w: 38.4,
                h: 38.4,
            },
            scale: 1.92,
            ink_local: (0.0, 0.0, 20.0, 20.0),
            flags: 0,
        }
    }

    fn meta(id: u32) -> IconMeta {
        IconMeta {
            id,
            id_hex: format!("{id:032x}"),
            sheet_stem: "shapes".to_string(),
            preset: "mono-clean".to_string(),
            colours: 2,
        }
    }

    #[test]
    fn the_default_pattern_names_the_first_row_predictably() {
        let row = derive_row(
            &meta(1),
            &placement(1),
            &metrics(20.0, 4.0, 0.5),
            16,
            "svg/shapes-001.svg",
            &CsvOptions::default(),
        );
        assert_eq!(row.index, 1);
        assert_eq!(row.name, "shapes-001");
        assert_eq!(row.slug, "shapes-001");
        assert_eq!(row.row, 1);
        assert_eq!(row.col, 1);
        assert_eq!(row.width, 20);
        // 38.4 px of target ink, to float precision.
        assert!((row.ink - 38.4).abs() < 1e-4, "{}", row.ink);
        // stroke/side = 4/20 = 0.20: hollow, and neither thin nor thick.
        assert_eq!(row.tags, "hollow even ink mono-clean");
    }

    #[test]
    fn grid_positions_are_one_based_and_wrap() {
        for (id, expect) in [(1u32, (1u32, 1u32)), (2, (1, 2)), (3, (1, 3)), (4, (2, 1))] {
            assert_eq!(grid_position(&placement(id), 3), expect);
        }
        // A zero-column grid must not divide by zero.
        assert_eq!(grid_position(&placement(1), 0), (1, 1));
    }

    #[test]
    fn patterns_expand_every_token_and_leave_typos_alone() {
        let values = PatternValues {
            sheet: "shapes",
            index: 7,
            row: 2,
            col: 3,
            preset: "pixel",
        };
        assert_eq!(expand_pattern("{sheet}-{index:03}", &values), "shapes-007");
        assert_eq!(expand_pattern("{sheet}/{row}x{col}", &values), "shapes/2x3");
        assert_eq!(expand_pattern("{preset}_{index}", &values), "pixel_7");
        assert_eq!(expand_pattern("{index:2}", &values), "07");
        assert_eq!(expand_pattern("{nope}", &values), "{nope}");
        assert_eq!(expand_pattern("plain", &values), "plain");
        assert_eq!(expand_pattern("open{index", &values), "open{index");
        assert_eq!(expand_pattern("", &values), "");
    }

    #[test]
    fn slugs_are_safe_and_stable() {
        assert_eq!(slugify("shapes-001"), "shapes-001");
        assert_eq!(slugify("Arrow Up.svg"), "arrow-up-svg");
        assert_eq!(slugify("  multi   space  "), "multi-space");
        assert_eq!(slugify("--"), "icon");
        assert_eq!(slugify(""), "icon");
        assert_eq!(slugify("a/b\\c:d"), "a-b-c-d");
    }

    #[test]
    fn tags_follow_the_measured_geometry() {
        assert!(derive_tags("mono", &metrics(100.0, 30.0, 1.0)).starts_with("solid thick"));
        assert!(derive_tags("mono", &metrics(100.0, 5.0, 0.3)).starts_with("hollow thin"));
        assert!(derive_tags("colour", &metrics(100.0, 25.0, 0.7)).starts_with("mixed even"));
        assert!(derive_tags("mono", &metrics(0.0, 0.0, 0.0)).contains("empty"));
    }

    #[test]
    fn the_writer_and_the_reader_round_trip_including_awkward_fields() {
        let mut row = derive_row(
            &meta(2),
            &placement(2),
            &metrics(20.0, 4.0, 0.5),
            16,
            "svg/x.svg",
            &CsvOptions {
                name_pattern: "a, \"quoted\" name\nwith newline".to_string(),
                ..CsvOptions::default()
            },
        );
        row.tags = "one two".to_string();
        let text = write_csv(&[row], &CSV_COLUMNS, &CsvOptions::default()).expect("writes");
        // RFC 4180 line endings, and the awkward name is quoted with doubled
        // quotes and its real newline inside the quotes.
        assert!(text.contains("\r\n"));
        assert!(text.contains("\"a, \"\"quoted\"\" name\nwith newline\""));
        let rows = parse_csv(&text, b',').expect("parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "index");
        assert_eq!(rows[0].len(), CSV_COLUMNS.len());
        assert_eq!(rows[1][1], "a, \"quoted\" name\nwith newline");
        assert_eq!(rows[1][2], "a-quoted-name-with-newline");
    }

    #[test]
    fn every_column_can_be_written_and_read_back() {
        let row = derive_row(
            &meta(3),
            &placement(3),
            &metrics(12.0, 3.0, 0.25),
            4,
            "svg/three.svg",
            &CsvOptions::default(),
        );
        let text = write_csv(&[row], &Column::ALL, &CsvOptions::default()).expect("writes");
        let rows = parse_csv(&text, b',').expect("parses");
        assert_eq!(rows[0].len(), Column::ALL.len());
        for (i, column) in Column::ALL.iter().enumerate() {
            assert_eq!(rows[0][i], column.header());
            assert_eq!(Column::parse(column.header()), Some(*column));
            assert_eq!(
                Column::parse(&column.header().to_uppercase()),
                Some(*column)
            );
        }
        assert_eq!(rows[1][Column::Preset as usize], "mono-clean");
        assert_eq!(rows[1][Column::Colours as usize], "2");
        assert_eq!(rows[1][Column::Id as usize], format!("{:032x}", 3));
        assert_eq!(Column::parse("nonsense"), None);
    }

    #[test]
    fn a_semicolon_wizard_export_is_still_a_csv() {
        let row = derive_row(
            &meta(1),
            &placement(1),
            &metrics(20.0, 4.0, 0.5),
            1,
            "svg/one.svg",
            &CsvOptions::default(),
        );
        let options = CsvOptions {
            delimiter: b';',
            header: false,
            ..CsvOptions::default()
        };
        let text =
            write_csv(&[row], &[Column::Name, Column::Row, Column::Col], &options).expect("writes");
        assert_eq!(text, "shapes-001;1;1\r\n");
        assert_eq!(
            parse_csv(&text, b';').expect("parses")[0],
            vec!["shapes-001", "1", "1"]
        );
    }

    #[test]
    fn a_headerless_export_has_no_header() {
        let row = derive_row(
            &meta(1),
            &placement(1),
            &metrics(20.0, 4.0, 0.5),
            1,
            "svg/one.svg",
            &CsvOptions::default(),
        );
        let options = CsvOptions {
            header: false,
            ..CsvOptions::default()
        };
        let text = write_csv(&[row], &CSV_COLUMNS, &options).expect("writes");
        assert_eq!(text.lines().count(), 1);
        assert!(text.starts_with("1,shapes-001,"));
    }

    #[test]
    fn a_truncated_quoted_field_is_refused() {
        assert_eq!(
            parse_csv("a,\"unterminated", b','),
            Err(CsvError::UnterminatedQuote { at: 2 })
        );
        assert_eq!(parse_csv("x", 0xff), Err(CsvError::BadDelimiter));
        let bad = CsvOptions {
            delimiter: b'\n',
            ..CsvOptions::default()
        };
        assert_eq!(
            write_csv(&[], &CSV_COLUMNS, &bad),
            Err(CsvError::BadDelimiter)
        );
    }

    #[test]
    fn empty_lines_between_rows_are_rows() {
        let rows = parse_csv("a,b\r\n\r\nc,d\r\n", b',').expect("parses");
        assert_eq!(rows, vec![vec!["a", "b"], vec![""], vec!["c", "d"]]);
        assert_eq!(parse_csv("", b',').expect("parses").len(), 0);
    }

    #[test]
    fn artwork_coverage_is_checked_before_writing_anything() {
        let art = vec![Artwork::empty(1, "a")];
        assert!(check_artwork_covers(&[placement(1)], &art).is_ok());
        assert_eq!(
            check_artwork_covers(&[placement(1), placement(2)], &art),
            Err(ExportError::MissingArtwork { id: 2 })
        );
    }
}
