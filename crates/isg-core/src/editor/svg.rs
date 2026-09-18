//! SVG import: path data, colours, transforms and the document scan.
//!
//! The editor imports icons — from the vectorizer, from a user's file — as
//! geometry, so what is needed here is the *drawing* subset of SVG, parsed
//! strictly: every command of the path grammar (`M L H V C S Q T A Z`, absolute
//! and relative), the `transform` attribute on paths and on the `<g>`s that
//! contain them, and hex fills. Anything else — gradients, CSS `style`
//! attributes, filters, text — is ignored rather than guessed at, and a path the
//! grammar does not accept is refused with a reason instead of being silently
//! mis-read into geometry the user cannot see coming.
//!
//! Two conversions happen on the way in, because the model stores cubics only:
//! quadratic segments become cubics (`Q`/`T`), and arcs become cubic chains
//! (≤90° each). The reflection rules of `S` and `T` are honoured, so a path
//! produced by a tool that uses shorthand smooth curves comes in intact.
//!
//! The parser is zero-dependency like the rest of `isg-core`: the scan is a
//! hand-written walk over the text, not an XML library.

use super::affine::Affine;
use super::geom::{Point, Subpath};

/// The `viewBox` and viewport of an `<svg>` element, when it declares them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewBox {
    /// `viewBox="x y width height"`.
    pub rect: [f32; 4],
    /// The `width` attribute, in user units (a percentage is not a number).
    pub width: Option<f32>,
    /// The `height` attribute, in user units.
    pub height: Option<f32>,
}

/// One `<path>` of an SVG, with the transforms that apply to it.
#[derive(Clone, Debug, PartialEq)]
pub struct SvgShape {
    /// The path's subpaths, in the SVG's own coordinate system.
    pub path: Vec<Subpath>,
    /// The `fill` attribute when it is a hex colour.
    pub fill: Option<[u8; 4]>,
    /// The path's own `transform`, composed with every inherited `<g>` one.
    pub transform: Affine,
}

/// Everything an SVG contributed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SvgDoc {
    /// The root `<svg>`'s viewBox, if it has one.
    pub view_box: Option<ViewBox>,
    /// One shape per `<path>` element, in document order.
    pub shapes: Vec<SvgShape>,
}

/// Why an SVG could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgError {
    /// A path command letter outside the SVG path grammar.
    UnknownCommand(char),
    /// A number was missing, or a character was neither a command nor a number.
    BadNumber,
    /// Path data that does not start with a moveto.
    ExpectedMoveto,
    /// A `<` or a quote that never closes.
    Truncated,
    /// A `transform` attribute that is not a transform list.
    BadTransform,
}

impl SvgError {
    /// A short message for the status line.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::UnknownCommand(_) => "unsupported path command",
            Self::BadNumber => "malformed path data",
            Self::ExpectedMoveto => "path data must start with a moveto",
            Self::Truncated => "truncated svg",
            Self::BadTransform => "malformed transform attribute",
        }
    }
}

// ---------------------------------------------------------------------------
// Path data
// ---------------------------------------------------------------------------

/// One token of a `d` attribute.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Token {
    Command(char),
    Number(f32),
}

/// Splits path data into commands and numbers.
///
/// Letters are recognised generally and validated by the parser, never dropped:
/// a tokenizer that only knew the supported commands would skip an `A` and
/// mis-read the rest of the path as if it were a different shape.
fn tokenize(d: &str) -> Result<Vec<Token>, SvgError> {
    let bytes = d.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte.is_ascii_alphabetic() {
            tokens.push(Token::Command(byte as char));
            i += 1;
            continue;
        }
        if byte.is_ascii_whitespace() || byte == b',' {
            i += 1;
            continue;
        }
        let start = i;
        if byte == b'+' || byte == b'-' {
            i += 1;
        }
        let mut digits = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            digits = true;
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                digits = true;
                i += 1;
            }
        }
        if !digits {
            return Err(SvgError::BadNumber);
        }
        if i < bytes.len() && (bytes[i] | 0x20) == b'e' {
            let mut j = i + 1;
            if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                j += 1;
            }
            let mut exponent = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                exponent = true;
                j += 1;
            }
            if exponent {
                i = j;
            }
        }
        let value = d[start..i]
            .parse::<f32>()
            .map_err(|_| SvgError::BadNumber)?;
        if !value.is_finite() {
            return Err(SvgError::BadNumber);
        }
        tokens.push(Token::Number(value));
    }
    Ok(tokens)
}

/// The cursor plus the state SVG's relative commands need.
struct PathParser {
    tokens: Vec<Token>,
    index: usize,
    /// The current point.
    current: Point,
    /// The current subpath's start point (the target of `Z`).
    start: Point,
    /// The second control point of the last cubic, for `S`'s reflection.
    last_cubic: Option<Point>,
    /// The control point of the last quadratic, for `T`'s reflection.
    last_quad: Option<Point>,
}

impl PathParser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            index: 0,
            current: Point::new(0.0, 0.0),
            start: Point::new(0.0, 0.0),
            last_cubic: None,
            last_quad: None,
        }
    }

    fn number(&mut self) -> Result<f32, SvgError> {
        match self.tokens.get(self.index) {
            Some(Token::Number(value)) => {
                self.index += 1;
                Ok(*value)
            }
            _ => Err(SvgError::BadNumber),
        }
    }

    fn point(&mut self, relative: bool) -> Result<Point, SvgError> {
        let raw = Point::new(self.number()?, self.number()?);
        Ok(if relative {
            Point::new(self.current.x + raw.x, self.current.y + raw.y)
        } else {
            raw
        })
    }

    fn has_number(&self) -> bool {
        matches!(self.tokens.get(self.index), Some(Token::Number(_)))
    }

    fn has_command(&self) -> bool {
        matches!(self.tokens.get(self.index), Some(Token::Command(_)))
    }

    /// Pushes a cubic, keeping the reflection state in step.
    fn cubic(&mut self, sub: &mut Subpath, c1: Point, c2: Point, to: Point) {
        sub.push_cubic(c1, c2, to);
        self.current = to;
        self.last_cubic = Some(c2);
        self.last_quad = None;
    }
}

/// Parses a `d` attribute into subpaths.
///
/// # Errors
///
/// Returns an [`SvgError`] for a command outside the grammar, malformed or
/// missing numbers, or data that does not begin with a moveto.
pub fn parse_path_data(d: &str) -> Result<Vec<Subpath>, SvgError> {
    let mut parser = PathParser::new(tokenize(d)?);
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut open: Option<usize> = None;
    let mut command: Option<char> = None;

    while parser.index < parser.tokens.len() {
        match parser.tokens[parser.index] {
            Token::Command(letter) => {
                command = Some(letter);
                parser.index += 1;
            }
            Token::Number(_) => {
                // A repeated coordinate set repeats the command — except after a
                // moveto, where the repeats are implicit linetos.
                match command {
                    None => return Err(SvgError::ExpectedMoveto),
                    Some('M') => command = Some('L'),
                    Some('m') => command = Some('l'),
                    Some(_) => {}
                }
            }
        }
        let Some(letter) = command else {
            return Err(SvgError::ExpectedMoveto);
        };
        let relative = letter.is_ascii_lowercase();
        let upper = letter.to_ascii_uppercase();
        // Every drawing command needs an open subpath: path data that reaches
        // one before its moveto is malformed, not implicitly positioned.
        let sub_of = |_subpaths: &mut Vec<Subpath>,
                      open: Option<usize>|
         -> Result<usize, SvgError> { open.ok_or(SvgError::ExpectedMoveto) };
        match upper {
            'M' => {
                let at = parser.point(relative)?;
                parser.current = at;
                parser.start = at;
                subpaths.push(Subpath::new(at));
                open = Some(subpaths.len() - 1);
                parser.last_cubic = None;
                parser.last_quad = None;
            }
            'L' => {
                let at = parser.point(relative)?;
                let index = sub_of(&mut subpaths, open)?;
                subpaths[index].push_line(at);
                parser.current = at;
                parser.last_cubic = None;
                parser.last_quad = None;
            }
            'H' => {
                let x = parser.number()?;
                let index = sub_of(&mut subpaths, open)?;
                let at = Point::new(
                    if relative { parser.current.x + x } else { x },
                    parser.current.y,
                );
                subpaths[index].push_line(at);
                parser.current = at;
                parser.last_cubic = None;
                parser.last_quad = None;
            }
            'V' => {
                let y = parser.number()?;
                let index = sub_of(&mut subpaths, open)?;
                let at = Point::new(
                    parser.current.x,
                    if relative { parser.current.y + y } else { y },
                );
                subpaths[index].push_line(at);
                parser.current = at;
                parser.last_cubic = None;
                parser.last_quad = None;
            }
            'C' => {
                let c1 = parser.point(relative)?;
                let c2 = parser.point(relative)?;
                let to = parser.point(relative)?;
                let index = sub_of(&mut subpaths, open)?;
                let mut sub = subpaths[index].clone();
                parser.cubic(&mut sub, c1, c2, to);
                subpaths[index] = sub;
            }
            'S' => {
                // The first control point mirrors the previous cubic's second
                // one about the current point; without a previous cubic it is
                // the current point itself.
                let c2 = parser.point(relative)?;
                let to = parser.point(relative)?;
                let current = parser.current;
                let c1 = match parser.last_cubic {
                    Some(previous) => {
                        Point::new(2.0 * current.x - previous.x, 2.0 * current.y - previous.y)
                    }
                    None => current,
                };
                let index = sub_of(&mut subpaths, open)?;
                let mut sub = subpaths[index].clone();
                parser.cubic(&mut sub, c1, c2, to);
                subpaths[index] = sub;
            }
            'Q' => {
                let control = parser.point(relative)?;
                let to = parser.point(relative)?;
                let (c1, c2) = quad_to_cubic(parser.current, control, to);
                let index = sub_of(&mut subpaths, open)?;
                let mut sub = subpaths[index].clone();
                sub.push_cubic(c1, c2, to);
                subpaths[index] = sub;
                parser.current = to;
                parser.last_quad = Some(control);
                parser.last_cubic = Some(c2);
            }
            'T' => {
                let to = parser.point(relative)?;
                let current = parser.current;
                let control = match parser.last_quad {
                    Some(previous) => {
                        Point::new(2.0 * current.x - previous.x, 2.0 * current.y - previous.y)
                    }
                    None => current,
                };
                let (c1, c2) = quad_to_cubic(current, control, to);
                let index = sub_of(&mut subpaths, open)?;
                let mut sub = subpaths[index].clone();
                sub.push_cubic(c1, c2, to);
                subpaths[index] = sub;
                parser.current = to;
                parser.last_quad = Some(control);
                parser.last_cubic = Some(c2);
            }
            'A' => {
                let rx = parser.number()?;
                let ry = parser.number()?;
                let rotation = parser.number()?;
                let large = parser.number()? != 0.0;
                let sweep = parser.number()? != 0.0;
                let to = parser.point(relative)?;
                let index = sub_of(&mut subpaths, open)?;
                let from = parser.current;
                let mut sub = subpaths[index].clone();
                for (c1, c2, end) in arc_to_cubics(from, rx, ry, rotation, large, sweep, to) {
                    sub.push_cubic(c1, c2, end);
                }
                if from == to {
                    // The spec drops an arc whose end point is its start.
                    parser.current = to;
                }
                subpaths[index] = sub;
                parser.current = to;
                parser.last_cubic = None;
                parser.last_quad = None;
            }
            'Z' => {
                let index = sub_of(&mut subpaths, open)?;
                subpaths[index].closed = true;
                parser.current = parser.start;
                parser.last_cubic = None;
                parser.last_quad = None;
            }
            other => return Err(SvgError::UnknownCommand(other)),
        }
        // Every command except `Z` consumes numbers; a command with none is a
        // mistake, and saying so beats emitting a degenerate segment.
        if upper != 'Z'
            && !parser.has_number()
            && !parser.has_command()
            && parser.index < parser.tokens.len()
        {
            return Err(SvgError::BadNumber);
        }
    }
    Ok(subpaths)
}

/// The cubic equivalent of one quadratic segment.
fn quad_to_cubic(from: Point, control: Point, to: Point) -> (Point, Point) {
    let c1 = Point::new(
        from.x + 2.0 / 3.0 * (control.x - from.x),
        from.y + 2.0 / 3.0 * (control.y - from.y),
    );
    let c2 = Point::new(
        to.x + 2.0 / 3.0 * (control.x - to.x),
        to.y + 2.0 / 3.0 * (control.y - to.y),
    );
    (c1, c2)
}

/// One elliptical arc as a chain of cubics, by the endpoint parameterisation of
/// SVG's implementation notes, with each piece covering at most a quarter turn.
fn arc_to_cubics(
    from: Point,
    rx: f32,
    ry: f32,
    rotation: f32,
    large_arc: bool,
    sweep: bool,
    to: Point,
) -> Vec<(Point, Point, Point)> {
    if from == to {
        return Vec::new();
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 {
        // A degenerate radius is a straight line, per the spec.
        return vec![(from, to, to)];
    }
    let phi = rotation.to_radians();
    let (sin_phi, cos_phi) = (phi.sin(), phi.cos());
    let dx = (from.x - to.x) / 2.0;
    let dy = (from.y - to.y) / 2.0;
    let x1 = cos_phi * dx + sin_phi * dy;
    let y1 = -sin_phi * dx + cos_phi * dy;
    // Grow the radii when they are too small to span the chord.
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let scale = lambda.sqrt();
        rx *= scale;
        ry *= scale;
    }
    let sign = if large_arc != sweep { 1.0 } else { -1.0 };
    let numerator = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let denominator = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let coefficient = if denominator == 0.0 {
        0.0
    } else {
        sign * (numerator.max(0.0) / denominator).sqrt()
    };
    let cx1 = coefficient * rx * y1 / ry;
    let cy1 = -coefficient * ry * x1 / rx;
    let center = Point::new(
        cos_phi * cx1 - sin_phi * cy1 + (from.x + to.x) / 2.0,
        sin_phi * cx1 + cos_phi * cy1 + (from.y + to.y) / 2.0,
    );
    let start_angle = angle_between(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut sweep_angle = angle_between(
        (x1 - cx1) / rx,
        (y1 - cy1) / ry,
        (-x1 - cx1) / rx,
        (-y1 - cy1) / ry,
    );
    if !sweep && sweep_angle > 0.0 {
        sweep_angle -= std::f32::consts::TAU;
    } else if sweep && sweep_angle < 0.0 {
        sweep_angle += std::f32::consts::TAU;
    }
    let pieces = (sweep_angle.abs() / std::f32::consts::FRAC_PI_2)
        .ceil()
        .max(1.0) as usize;
    let step = sweep_angle / pieces as f32;
    let mut out = Vec::with_capacity(pieces);
    let mut angle = start_angle;
    for _ in 0..pieces {
        let next = angle + step;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        let (sin_b, cos_b) = (next.sin(), next.cos());
        let at = |sin: f32, cos: f32| {
            Point::new(
                center.x + rx * cos * cos_phi - ry * sin * sin_phi,
                center.y + rx * cos * sin_phi + ry * sin * cos_phi,
            )
        };
        let point_a = at(sin_a, cos_a);
        let point_b = at(sin_b, cos_b);
        // Tangents of the ellipse's parametric form, in user space.
        let derivative = |sin: f32, cos: f32| {
            Point::new(
                -rx * sin * cos_phi - ry * cos * sin_phi,
                -rx * sin * sin_phi + ry * cos * cos_phi,
            )
        };
        let (da, db) = (derivative(sin_a, cos_a), derivative(sin_b, cos_b));
        out.push((
            Point::new(point_a.x + k * da.x, point_a.y + k * da.y),
            Point::new(point_b.x - k * db.x, point_b.y - k * db.y),
            point_b,
        ));
        angle = next;
    }
    // The chain must land exactly on the requested end point: the arithmetic
    // above is accurate but not bit-exact, and a path that misses its own end
    // by an epsilon would not stitch to what follows it.
    if let Some(last) = out.last_mut() {
        last.2 = to;
    }
    out
}

/// The signed angle between two vectors, in radians, in `(-π, π]`.
fn angle_between(ux: f32, uy: f32, vx: f32, vy: f32) -> f32 {
    (ux * vy - uy * vx).atan2(ux * vx + uy * vy)
}

// ---------------------------------------------------------------------------
// Colours and transforms
// ---------------------------------------------------------------------------

/// Parses `#rgb`, `#rrggbb` and `#rrggbbaa` (with or without the `#`).
#[must_use]
pub fn hex_color(value: &str) -> Option<[u8; 4]> {
    let hex = value.trim().trim_start_matches('#');
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) || hex.is_empty() {
        return None;
    }
    let part = |start: usize, end: usize| u8::from_str_radix(&hex[start..end], 16).ok();
    match hex.len() {
        3 => Some([expand(hex, 0)?, expand(hex, 1)?, expand(hex, 2)?, 255]),
        4 => Some([
            expand(hex, 0)?,
            expand(hex, 1)?,
            expand(hex, 2)?,
            expand(hex, 3)?,
        ]),
        6 => Some([part(0, 2)?, part(2, 4)?, part(4, 6)?, 255]),
        8 => Some([part(0, 2)?, part(2, 4)?, part(4, 6)?, part(6, 8)?]),
        _ => None,
    }
}

fn expand(hex: &str, index: usize) -> Option<u8> {
    let digit = hex.as_bytes().get(index).copied()? as char;
    u8::from_str_radix(&format!("{digit}{digit}"), 16).ok()
}

/// Splits a `transform` attribute into its function calls.
///
/// # Errors
///
/// Returns [`SvgError::BadTransform`] for a call without its closing parenthesis
/// or for text left over between calls — a transform that cannot be read is
/// reported, never silently treated as the identity.
fn transform_calls(value: &str) -> Result<Vec<(&str, Vec<f32>)>, SvgError> {
    let mut out: Vec<(&str, Vec<f32>)> = Vec::new();
    let mut rest = value;
    loop {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            return Ok(out);
        }
        let Some(open) = rest.find('(') else {
            return Err(SvgError::BadTransform);
        };
        let name = rest[..open].trim().trim_end_matches(',').trim();
        let Some(close) = rest[open..].find(')') else {
            return Err(SvgError::BadTransform);
        };
        let body = &rest[open + 1..open + close];
        let mut args: Vec<f32> = Vec::new();
        for part in body.split(|c: char| c == ',' || c.is_whitespace()) {
            if part.is_empty() {
                continue;
            }
            args.push(part.parse::<f32>().map_err(|_| SvgError::BadTransform)?);
        }
        out.push((name, args));
        rest = &rest[open + close + 1..];
    }
}

/// Parses a `transform` attribute into one affine.
///
/// The leftmost function is the outermost: `translate(10) scale(2)` means a
/// scale inside a translation, so a point is scaled first and then moved — the
/// same as nesting `<g transform="translate(10)"><g transform="scale(2)">`.
///
/// # Errors
///
/// Returns [`SvgError::BadTransform`] when a call is unknown or short of
/// arguments; an empty attribute is the identity.
pub fn parse_transform(value: &str) -> Result<Affine, SvgError> {
    let mut result = Affine::IDENTITY;
    for (name, args) in transform_calls(value)? {
        let at = |i: usize| args.get(i).copied();
        let matrix = match name {
            "translate" => {
                let (Some(tx), ty) = (at(0), at(1)) else {
                    return Err(SvgError::BadTransform);
                };
                Affine::translate(tx, ty.unwrap_or(0.0))
            }
            "scale" => {
                let (Some(sx), sy) = (at(0), at(1)) else {
                    return Err(SvgError::BadTransform);
                };
                Affine::scale(sx, sy.unwrap_or(sx))
            }
            "rotate" => {
                let Some(degrees) = at(0) else {
                    return Err(SvgError::BadTransform);
                };
                match (at(1), at(2)) {
                    (Some(cx), Some(cy)) => Affine::translate(-cx, -cy)
                        .then(Affine::rotate(degrees))
                        .then(Affine::translate(cx, cy)),
                    _ => Affine::rotate(degrees),
                }
            }
            "matrix" => {
                let Some(m) = (0..6).map(at).collect::<Option<Vec<f32>>>() else {
                    return Err(SvgError::BadTransform);
                };
                Affine::new([m[0], m[1], m[2], m[3], m[4], m[5]])
            }
            "skewX" | "skewY" => {
                let Some(degrees) = at(0) else {
                    return Err(SvgError::BadTransform);
                };
                let tangent = degrees.to_radians().tan();
                if name == "skewX" {
                    Affine::new([1.0, 0.0, tangent, 1.0, 0.0, 0.0])
                } else {
                    Affine::new([1.0, tangent, 0.0, 1.0, 0.0, 0.0])
                }
            }
            "" => continue,
            _ => return Err(SvgError::BadTransform),
        };
        // The new call is *inner* relative to what has been read so far.
        result = matrix.then(result);
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// The document scan
// ---------------------------------------------------------------------------

/// A tag found in the text.
struct Tag<'a> {
    name: &'a str,
    body: &'a str,
    closing: bool,
    self_closing: bool,
}

/// Reads the next tag from `text` at or after `from`.
///
/// Comments, declarations and processing instructions are skipped. The returned
/// tag borrows from the input; attribute values are not unescaped (no exporter
/// of icon geometry writes entities into path data).
fn next_tag(text: &str, from: usize) -> Result<Option<(Tag<'_>, usize)>, SvgError> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if text[i..].starts_with("<!--") {
            let Some(end) = text[i..].find("-->") else {
                return Err(SvgError::Truncated);
            };
            i += end + 3;
            continue;
        }
        if text[i..].starts_with("<!") || text[i..].starts_with("<?") {
            let Some(end) = text[i..].find('>') else {
                return Err(SvgError::Truncated);
            };
            i += end + 1;
            continue;
        }
        // Find the tag's `>`, ignoring any inside a quoted attribute value.
        let mut j = i + 1;
        let mut quote: Option<u8> = None;
        while j < bytes.len() {
            match bytes[j] {
                b'"' | b'\'' => {
                    if quote == Some(bytes[j]) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(bytes[j]);
                    }
                }
                b'>' if quote.is_none() => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            return Err(SvgError::Truncated);
        }
        let raw = &text[i + 1..j];
        let closing = raw.starts_with('/');
        let body = raw.trim_start_matches('/').trim();
        let self_closing = body.ends_with('/');
        let body = body.trim_end_matches('/').trim();
        let name_end = body
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(body.len());
        return Ok(Some((
            Tag {
                name: &body[..name_end],
                body: &body[name_end..],
                closing,
                self_closing,
            },
            j + 1,
        )));
    }
    Ok(None)
}

/// Reads one attribute, or `None`.
fn attribute<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Match the name on a word boundary.
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'-');
        if boundary && body[i..].starts_with(name) {
            let mut j = i + name.len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if bytes.get(j) == Some(&b'=') {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let quote = *bytes.get(j)?;
                if quote == b'"' || quote == b'\'' {
                    let start = j + 1;
                    let end = start + body[start..].find(quote as char)?;
                    return Some(&body[start..end]);
                }
            }
        }
        i += 1;
    }
    None
}

/// Parses a length like `24`, `24px` or `1.5em`; a percentage is not a length.
fn parse_length(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    if trimmed.ends_with('%') {
        return None;
    }
    let number: String = trimmed
        .chars()
        .take_while(|c| {
            c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+' || *c == 'e' || *c == 'E'
        })
        .collect();
    number.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Parses an SVG document's drawing content.
///
/// # Errors
///
/// Returns an [`SvgError`] when the text is truncated or a path (or one of its
/// transforms) is malformed. Unknown *elements* are ignored: real files carry
/// plenty of markup that has nothing to do with the geometry.
pub fn parse(text: &str) -> Result<SvgDoc, SvgError> {
    let mut doc = SvgDoc::default();
    // The stack of inherited group transforms; the root is the identity.
    let mut stack: Vec<Affine> = vec![Affine::IDENTITY];
    let mut cursor = 0;
    while let Some((tag, end)) = next_tag(text, cursor)? {
        cursor = end;
        match (tag.name, tag.closing) {
            ("svg", false) => {
                if let Some(value) = attribute(tag.body, "viewBox") {
                    let numbers: Vec<f32> = value
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|part| !part.is_empty())
                        .filter_map(|part| part.parse::<f32>().ok())
                        .collect();
                    if numbers.len() == 4 {
                        doc.view_box = Some(ViewBox {
                            rect: [numbers[0], numbers[1], numbers[2], numbers[3]],
                            width: attribute(tag.body, "width").and_then(parse_length),
                            height: attribute(tag.body, "height").and_then(parse_length),
                        });
                    }
                }
            }
            ("g", false) => {
                let inherited = *stack.last().unwrap_or(&Affine::IDENTITY);
                let own = match attribute(tag.body, "transform") {
                    Some(value) => parse_transform(value)?,
                    None => Affine::IDENTITY,
                };
                if tag.self_closing {
                    continue;
                }
                // The group's transform is applied before the ones inherited
                // from the groups around it.
                stack.push(own.then(inherited));
            }
            ("g", true) => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            ("path", false) => {
                let Some(d) = attribute(tag.body, "d") else {
                    continue;
                };
                // A subpath with no segments (`<path d="M1,1"/>`) draws nothing,
                // so it is not geometry: keeping it would put an unselectable,
                // uneditable point in the document.
                let path: Vec<Subpath> = parse_path_data(d)?
                    .into_iter()
                    .filter(|sub| !sub.segs.is_empty())
                    .collect();
                if path.is_empty() {
                    continue;
                }
                let inherited = *stack.last().unwrap_or(&Affine::IDENTITY);
                let own = match attribute(tag.body, "transform") {
                    Some(value) => parse_transform(value)?,
                    None => Affine::IDENTITY,
                };
                doc.shapes.push(SvgShape {
                    path,
                    fill: attribute(tag.body, "fill").and_then(hex_color),
                    transform: own.then(inherited),
                });
            }
            _ => {}
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn points(path: &[Subpath]) -> Vec<Point> {
        let mut out = Vec::new();
        for sub in path {
            out.push(sub.start);
            for seg in &sub.segs {
                out.push(seg.end());
            }
        }
        out
    }

    #[test]
    fn absolute_lines_and_close() {
        let path = parse_path_data("M10,10 L20,10 L20,20 Z").unwrap();
        assert_eq!(path.len(), 1);
        assert!(path[0].closed);
        assert_eq!(path[0].start, Point::new(10.0, 10.0));
        let seen = points(&path);
        assert_eq!(
            seen,
            vec![
                Point::new(10.0, 10.0),
                Point::new(20.0, 10.0),
                Point::new(20.0, 20.0)
            ]
        );
    }

    #[test]
    fn relative_commands_and_implicit_lineto() {
        // After a moveto, extra coordinate pairs are linetos — same case here.
        let path = parse_path_data("m 5 5 10 0 0 10").unwrap();
        let seen = points(&path);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1], Point::new(15.0, 5.0));
        assert_eq!(seen[2], Point::new(15.0, 15.0));
        // A following absolute command switches back.
        let path = parse_path_data("m 5 5 10 0 L 0 0").unwrap();
        assert_eq!(points(&path)[2], Point::new(0.0, 0.0));
    }

    #[test]
    fn horizontal_and_vertical_lines_carry_the_other_axis_over() {
        let path = parse_path_data("M1,2 H10 v5 h-4 V0").unwrap();
        let seen = points(&path);
        assert_eq!(seen[1], Point::new(10.0, 2.0));
        assert_eq!(seen[2], Point::new(10.0, 7.0));
        assert_eq!(seen[3], Point::new(6.0, 7.0));
        assert_eq!(seen[4], Point::new(6.0, 0.0));
    }

    #[test]
    fn smooth_cubic_reflects_the_previous_control_point() {
        // c2 of the first cubic is (10,10); the reflection about (10,0) is
        // (10,-10).
        let path = parse_path_data("M0,0 C0,10 10,10 10,0 S20,-10 20,0").unwrap();
        let segs = &path[0].segs;
        assert_eq!(segs.len(), 2);
        let [c1, c2, to] = segs[1].control_points();
        assert!(close(c1.x, 10.0) && close(c1.y, -10.0), "{c1:?}");
        assert!(close(c2.x, 20.0) && close(c2.y, -10.0), "{c2:?}");
        assert!(close(to.x, 20.0) && close(to.y, 0.0));
        // Without a preceding cubic the reflection is the current point.
        let path = parse_path_data("M4,4 S8,8 12,4").unwrap();
        let [c1, _, _] = path[0].segs[0].control_points();
        assert!(close(c1.x, 4.0) && close(c1.y, 4.0), "{c1:?}");
    }

    #[test]
    fn quadratics_become_cubics_and_t_reflects() {
        let path = parse_path_data("M0,0 Q5,10 10,0 T20,0").unwrap();
        let segs = &path[0].segs;
        assert_eq!(segs.len(), 2);
        let [c1, c2, _] = segs[0].control_points();
        assert!(close(c1.x, 10.0 / 3.0) && close(c1.y, 20.0 / 3.0), "{c1:?}");
        assert!(close(c2.x, 20.0 / 3.0) && close(c2.y, 20.0 / 3.0), "{c2:?}");
        // T reflects the quadratic control point (5,10) about (10,0) → (15,-10).
        let [c1, c2, to] = segs[1].control_points();
        assert!(
            close(c1.x, 40.0 / 3.0) && close(c1.y, -20.0 / 3.0),
            "{c1:?}"
        );
        assert!(
            close(c2.x, 50.0 / 3.0) && close(c2.y, -20.0 / 3.0),
            "{c2:?}"
        );
        assert!(close(to.x, 20.0), "{to:?}");
    }

    #[test]
    fn an_arc_becomes_cubics_that_trace_the_ellipse() {
        // A half circle of radius 5 from (0,0) to (10,0). The sweep flag is the
        // direction of increasing angle, which in SVG's y-down space reads as
        // clockwise on screen: from the chord's left end that goes *up*, to
        // y = -5.
        let path = parse_path_data("M0,0 A5,5 0 0 1 10,0").unwrap();
        let flattened = path[0].flatten();
        let end = flattened.last().copied().unwrap();
        assert!(close(end.x, 10.0) && close(end.y, 0.0), "{end:?}");
        let (length, lowest) = measure(&flattened);
        let expected = std::f32::consts::PI * 5.0;
        assert!(
            (length - expected).abs() < expected * 0.01,
            "arc length {length} vs {expected}"
        );
        assert!(
            lowest < -4.9,
            "sweep 1 should bulge to y = -5, got {lowest}"
        );
        // The other sweep direction goes the other way.
        let down = parse_path_data("M0,0 A5,5 0 0 0 10,0").unwrap();
        let highest = down[0]
            .flatten()
            .iter()
            .fold(0.0f32, |acc, point| acc.max(point.y));
        assert!(
            highest > 4.9,
            "sweep 0 should bulge to y = +5, got {highest}"
        );

        // With a chord that is not a diameter the two arcs differ: radius 8 over
        // a chord of 10 spans 2·asin(5/8) = 1.3503 rad, and the large-arc variant
        // covers the rest of the circle.
        let chord = 10.0f32;
        let radius = 8.0f32;
        let minor_angle = 2.0 * (chord / 2.0 / radius).asin();
        let (minor, _) = measure(&parse_path_data("M0,0 A8,8 0 0 1 10,0").unwrap()[0].flatten());
        let (major, _) = measure(&parse_path_data("M0,0 A8,8 0 1 1 10,0").unwrap()[0].flatten());
        let (want_minor, want_major) = (
            radius * minor_angle,
            radius * (std::f32::consts::TAU - minor_angle),
        );
        assert!(
            (minor - want_minor).abs() < want_minor * 0.01,
            "minor arc {minor} vs {want_minor}"
        );
        assert!(
            (major - want_major).abs() < want_major * 0.01,
            "major arc {major} vs {want_major}"
        );
        // Both land on the requested end point.
        for d in ["M0,0 A8,8 0 0 1 10,0", "M0,0 A8,8 0 1 0 10,0"] {
            let path = parse_path_data(d).unwrap();
            let end = path[0].flatten().last().copied().unwrap();
            assert!(
                close(end.x, 10.0) && close(end.y, 0.0),
                "{d} ended at {end:?}"
            );
        }
    }

    /// Polyline length and lowest point of a flattened subpath.
    fn measure(points: &[Point]) -> (f32, f32) {
        let mut length = 0.0;
        let mut lowest = 0.0f32;
        for pair in points.windows(2) {
            length += pair[1].distance(pair[0]);
        }
        for point in points {
            lowest = lowest.min(point.y);
        }
        (length, lowest)
    }

    #[test]
    fn a_degenerate_arc_is_a_line() {
        let path = parse_path_data("M0,0 A0,5 0 0 1 10,10").unwrap();
        assert_eq!(path[0].segs.len(), 1);
        assert_eq!(path[0].segs[0].end(), Point::new(10.0, 10.0));
        // An arc that starts and ends at the same point is dropped entirely.
        let path = parse_path_data("M3,3 A5,5 0 0 1 3,3 L9,9").unwrap();
        assert_eq!(path[0].segs.len(), 1);
        assert_eq!(path[0].segs[0].end(), Point::new(9.0, 9.0));
    }

    #[test]
    fn multiple_subpaths_and_repeated_close() {
        let path = parse_path_data("M0,0 L5,0 Z M10,10 L15,10 Z z").unwrap();
        assert_eq!(path.len(), 2);
        assert!(path.iter().all(|sub| sub.closed));
        assert_eq!(path[1].start, Point::new(10.0, 10.0));
    }

    #[test]
    fn malformed_path_data_is_refused_with_a_reason() {
        assert_eq!(
            parse_path_data("M0,0 B10,10"),
            Err(SvgError::UnknownCommand('B'))
        );
        assert_eq!(
            parse_path_data("10,10 L20,20"),
            Err(SvgError::ExpectedMoveto)
        );
        assert_eq!(parse_path_data("M0,0 L"), Err(SvgError::BadNumber));
        assert_eq!(parse_path_data("M0,0 L x"), Err(SvgError::BadNumber));
        // A truncated exponent is a malformed number, not a valid "1".
        assert_eq!(parse_path_data("M0,0 L1e"), Err(SvgError::BadNumber));
        assert_eq!(
            SvgError::UnknownCommand('B').message(),
            "unsupported path command"
        );
        assert_eq!(SvgError::Truncated.message(), "truncated svg");
    }

    #[test]
    fn numbers_with_exponents_and_tight_separators() {
        let path = parse_path_data("M1e1,-1E1L20,20").unwrap();
        assert_eq!(path[0].start, Point::new(10.0, -10.0));
        // "10-5" is two numbers: the sign separates them.
        let path = parse_path_data("M0,0L10-5").unwrap();
        assert_eq!(path[0].segs[0].end(), Point::new(10.0, -5.0));
        let path = parse_path_data("M.5.5L1.5,2.5").unwrap();
        assert_eq!(path[0].start, Point::new(0.5, 0.5));
        assert_eq!(path[0].segs[0].end(), Point::new(1.5, 2.5));
    }

    #[test]
    fn colours() {
        assert_eq!(hex_color("#ff0000"), Some([255, 0, 0, 255]));
        assert_eq!(hex_color("00ff00"), Some([0, 255, 0, 255]));
        assert_eq!(hex_color("#abc"), Some([170, 187, 204, 255]));
        assert_eq!(hex_color("#11223344"), Some([17, 34, 51, 68]));
        assert_eq!(hex_color("none"), None);
        assert_eq!(hex_color("#12345"), None);
    }

    #[test]
    fn transforms_compose_in_order() {
        // translate then scale: the origin moves, then everything doubles.
        let m = parse_transform("translate(10,20) scale(2)").unwrap();
        assert_eq!(m.apply(0.0, 0.0), (10.0, 20.0));
        assert_eq!(m.apply(1.0, 0.0), (12.0, 20.0));
        // Rotate about a centre (clockwise in the document's y-down space).
        let m = parse_transform("rotate(90 10 10)").unwrap();
        let (x, y) = m.apply(20.0, 10.0);
        assert!(close(x, 10.0) && close(y, 20.0), "({x},{y})");
        assert_eq!(parse_transform("").unwrap(), Affine::IDENTITY);
        assert_eq!(
            parse_transform("scale(3)").unwrap().apply(2.0, 2.0),
            (6.0, 6.0)
        );
        assert_eq!(
            parse_transform("translate(5)").unwrap().apply(1.0, 1.0),
            (6.0, 1.0)
        );
        assert!(parse_transform("wobble(1)").is_err());
        assert!(parse_transform("translate(1,2,3").is_err());
        assert!(parse_transform("translate(1,2) junk").is_err());
        assert!(parse_transform("translate(1,2) scale(x)").is_err());
        assert!(parse_transform("translate 5").is_err());
    }

    #[test]
    fn a_document_is_scanned_for_paths_and_inherited_transforms() {
        let svg = r##"
            <?xml version="1.0"?>
            <!-- a comment with a <path d="M0,0 L1,1"/> that must be ignored -->
            <svg width="24px" height="24" viewBox="0 0 24 24">
              <g transform="translate(10,20)">
                <path d="M0,0 L2,0 L2,2 Z" fill="#ff0000"/>
                <g transform="scale(2)">
                  <path d="M1,1 L2,2" fill="#00ff00" transform="translate(1,0)"/>
                </g>
              </g>
              <path d="M5,5 L6,6"/>
              <circle cx="1" cy="1" r="1"/>
            </svg>"##;
        let doc = parse(svg).unwrap();
        assert_eq!(doc.shapes.len(), 3);
        let view = doc.view_box.unwrap();
        assert_eq!(view.rect, [0.0, 0.0, 24.0, 24.0]);
        assert_eq!(view.width, Some(24.0));
        assert_eq!(view.height, Some(24.0));

        // First path: translated by its group only.
        assert_eq!(doc.shapes[0].fill, Some([255, 0, 0, 255]));
        let (x, y) = doc.shapes[0].transform.apply(2.0, 2.0);
        assert!(close(x, 12.0) && close(y, 22.0), "({x},{y})");

        // Second: own translate, then the nested group's scale, then the outer
        // translate.
        assert_eq!(doc.shapes[1].fill, Some([0, 255, 0, 255]));
        let (x, y) = doc.shapes[1].transform.apply(1.0, 1.0);
        assert!(close(x, 14.0) && close(y, 22.0), "({x},{y})");

        // Third: no transform, no fill.
        assert_eq!(doc.shapes[2].fill, None);
        assert_eq!(doc.shapes[2].transform, Affine::IDENTITY);
    }

    #[test]
    fn unclosed_groups_and_truncated_files() {
        // An unclosed <g> leaves its transform in force — the file is still
        // readable, and refusing it would help nobody.
        let doc =
            parse(r##"<svg><g transform="translate(1,1)"><path d="M0,0 L1,1"/></svg>"##).unwrap();
        assert_eq!(doc.shapes.len(), 1);
        assert_eq!(doc.shapes[0].transform.apply(0.0, 0.0), (1.0, 1.0));
        // A tag that never closes is a truncation, and says so.
        assert_eq!(
            parse(r##"<svg><path d="M0,0 L1,1""##),
            Err(SvgError::Truncated)
        );
        assert_eq!(parse("<svg><path d='M0,0'"), Err(SvgError::Truncated));
        // A malformed path inside a document is refused, not skipped.
        assert_eq!(
            parse(r##"<svg><path d="M0,0 B1,1"/></svg>"##),
            Err(SvgError::UnknownCommand('B'))
        );
        // No paths at all is not an error, just an empty document.
        let doc = parse("<svg><rect x='0' y='0'/></svg>").unwrap();
        assert!(doc.shapes.is_empty());
        // Neither is a path with no drawing commands in it.
        let doc = parse(r##"<svg><path d="M1,1"/><path d="M2,2 L3,3"/></svg>"##).unwrap();
        assert_eq!(doc.shapes.len(), 1);
        assert_eq!(doc.shapes[0].path[0].start, Point::new(2.0, 2.0));
    }
}
