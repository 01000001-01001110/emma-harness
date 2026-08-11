//! Addressing a line by *what it says*, not only by where it sits.
//!
//! A line number on its own is a promise the filesystem never made. The model
//! reads line 12, thinks for four turns, and edits line 12 — but the user saved
//! a file in their editor in between, an import got added at the top, and line
//! 12 is now something else entirely. A line-numbered edit applies cleanly to
//! the wrong line and returns `Ok`. That is the same class of failure `Write`'s
//! read-before-write rule exists to stop, one line wide.
//!
//! So an address here is a line number **and a short hash of that line's
//! content**: `12#a3f9`. The number says where to look; the hash says what must
//! be there. If they disagree the edit is refused, and the refusal can say
//! something useful — including, often, where the line actually went.
//!
//! **Why a non-cryptographic hash is the right call.** FNV-1a is trivially
//! collidable by anyone who wants to. That does not matter, because the thing
//! being defended against is *accident*: a file that drifted, a model reusing a
//! stale line number. An attacker who can rewrite the file being edited already
//! has everything the collision would buy them. Paying for SHA-2 here would buy
//! a property nobody in this threat model needs, and would add a dependency to a
//! crate that currently has none for this.
//!
//! **Why the rendered hash is four hex characters and the stored one is 64
//! bits.** They defend different things and can afford different budgets. The
//! rendered hash is copied by the model on every addressed edit and printed on
//! every line of every `Read`, so its width is a token cost paid thousands of
//! times; 16 bits gives a 1-in-65536 chance that a *drifted* line is mistaken
//! for the original, which is small next to the chance the model gets the line
//! number wrong for ordinary reasons. The stored hash costs eight bytes in a
//! `HashMap` nobody pays tokens for, so it keeps all 64 bits — and because
//! `session.rs` checks the stored hash over the whole edited range while the
//! model only supplies the two endpoints, the endpoints end up checked twice, at
//! 16 bits by the model's claim and at 64 bits by the tracker's record. A
//! collision would have to happen in both.
//!
//! The fold in [`short`] is cheap insurance, and it is worth recording that it
//! was *measured* rather than assumed, because the obvious justification for it
//! turns out to be wrong. FNV-1a's lowest bit is a parity of the input bytes and
//! its low bits are conventionally described as weak, which suggests truncating
//! the hash directly would collide badly on near-identical source lines. Over a
//! corpus of every `.rs` and `.md` line in this repository — 50,621 distinct
//! lines — folded and unfolded truncation both land within 1% of the collision
//! count a uniform 16-bit hash would produce. The fold buys nothing measurable
//! here. It is kept because it costs two instructions, because the one bit that
//! *is* provably weak is a real bit out of sixteen, and because the corpus that
//! disproved the strong claim is not the only corpus this will ever see. What it
//! is not is a load-bearing part of the guarantee, and the test below asserts
//! only what it can actually demonstrate.
//!
//! One number that framing makes easy to misread: 19,531 colliding *pairs* in
//! that corpus sounds alarming and is irrelevant. Nothing here ever asks "do any
//! two lines in this file share a hash". It asks "does the line now at position
//! 12 hash to what the model expected at position 12", which is one comparison
//! against one value, and 1 in 65,536 is the whole of the exposure.

/// What `Read` prints in place of a hash for a line it had to clip.
///
/// A clipped line is one the model has only seen the front of, and replacing a
/// line you have not seen the end of destroys the end of it. Rather than omit
/// the hash — which would make the column ragged and invite the model to guess
/// — the slot is filled with something that is visibly not a hash and that
/// `Edit` refuses by name.
pub const CLIPPED: &str = "----";

/// FNV-1a over the line's bytes, with the line terminator excluded.
///
/// The terminator is excluded so that the same text hashes the same whether it
/// is the last line of the file or not, and whether the file is CRLF or LF.
/// Callers strip it; this function does not look for it. What *is* included is
/// leading and trailing whitespace, deliberately: trailing whitespace is real
/// content, a formatter stripping it is a real change to the line, and a hash
/// that ignored it would let exactly that change slip past unnoticed.
pub fn hash_line(line: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in line.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The four characters the model sees and quotes back. See the module doc for
/// why the value is folded before it is truncated.
pub fn short(hash: u64) -> String {
    let folded = hash ^ (hash >> 32);
    let folded = folded ^ (folded >> 16);
    format!("{:04x}", folded as u16)
}

/// One end of an address: the line the model is pointing at, and the four
/// characters it claims are there.
///
/// The claimed hash is kept as text rather than parsed to a number so that the
/// refusal can quote it back exactly as the model wrote it. `12#A3F9` and
/// `12#a3f9` compare equal — case is normalised on the way in — but a model
/// that mistyped a character sees its own typo in the error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub line: usize,
    pub hash: String,
}

impl Anchor {
    pub fn matches(&self, content: &str) -> bool {
        short(hash_line(content)) == self.hash
    }
}

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.line, self.hash)
    }
}

/// A whole address: one line, or an inclusive range of them.
///
/// A range names only its two endpoints. That is a deliberate token decision
/// and it is safe only because of what `session.rs` remembers: the interior
/// lines are checked against the hashes the harness recorded when it showed
/// them, so the model does not have to send thirty hashes to replace thirty
/// lines. See [`crate::session::ReadTracker::line_hashes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub start: Anchor,
    /// `None` for a single-line address. Kept separate from `start` rather than
    /// collapsed to `end == start`, because the two are reported differently
    /// and conflating them makes the messages lie about what was asked for.
    pub end: Option<Anchor>,
}

impl Address {
    pub fn first(&self) -> usize {
        self.start.line
    }

    pub fn last(&self) -> usize {
        self.end.as_ref().map(|a| a.line).unwrap_or(self.start.line)
    }

    /// The two endpoints, in order, for checking. One entry for a single line.
    pub fn ends(&self) -> Vec<&Anchor> {
        match &self.end {
            None => vec![&self.start],
            Some(end) => vec![&self.start, end],
        }
    }
}

/// Parse `12#a3f9` or `12#a3f9-15#b7c1`.
///
/// Every rejection here names what was wrong *and* shows the shape that would
/// have worked, because this is the one argument in the filesystem surface with
/// a syntax the model has to construct rather than copy wholesale, and a syntax
/// error with no example is a guess-and-retry loop. The errors are plain
/// `String`s so the caller can prefix them with the tool and parameter name in
/// the one spelling `args.rs` uses everywhere else.
pub fn parse(raw: &str) -> Result<Address, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("is empty; it should look like 12#a3f9, or 12#a3f9-15#b7c1 for a range".into());
    }

    // The range separator is a hyphen *followed by a digit*, and it has to be
    // identified that precisely because [`CLIPPED`] is four hyphens: splitting
    // on the first hyphen anywhere turns `12#----` into the range `12#` to
    // `---`, and the model gets told its hash is malformed instead of being
    // told the line was clipped. Neither a line number nor a hex hash can
    // contain a hyphen, so "hyphen, then digit" is unambiguous.
    let split = text
        .char_indices()
        .find(|(idx, c)| *c == '-' && text[idx + 1..].starts_with(|n: char| n.is_ascii_digit()));
    let (head, tail) = match split {
        None => (text, None),
        Some((idx, _)) => (&text[..idx], Some(&text[idx + 1..])),
    };

    let start = anchor(head, raw)?;
    let end = match tail {
        None => None,
        Some(t) => Some(anchor(t, raw)?),
    };

    if let Some(end) = &end {
        if end.line < start.line {
            return Err(format!(
                "is {raw}, which ends before it starts; a range runs from the lower line \
                 number to the higher one"
            ));
        }
    }

    Ok(Address { start, end })
}

fn anchor(part: &str, whole: &str) -> Result<Anchor, String> {
    let Some((num, hash)) = part.split_once('#') else {
        // The overwhelmingly likely mistake, and worth its own message: the
        // model wrote a bare line number. Telling it "syntax error" would send
        // it to re-read the description; telling it the hash is in the Read
        // output it already has sends it straight to the fix.
        return Err(format!(
            "is {whole}, which has no #hash; copy the whole `line#hash` label from the \
             left-hand column of Read's output — a line number on its own does not say \
             which text you meant"
        ));
    };

    let line: usize = num
        .trim()
        .parse()
        .map_err(|_| format!("is {whole}, whose line number {num:?} is not a whole number"))?;
    if line == 0 {
        return Err(format!("is {whole}, but line numbers start at 1"));
    }

    let hash = hash.trim().to_ascii_lowercase();
    if hash == CLIPPED {
        return Err(format!(
            "is {whole}, and {CLIPPED} is what Read prints when it had to clip a line that \
             was too long to show. You have not seen the end of that line, so replacing it \
             would discard text you never read; use old_string to change the part you did see"
        ));
    }
    if hash.len() != 4 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "is {whole}, whose hash {hash:?} is not the four hex characters Read prints"
        ));
    }

    Ok(Anchor { line, hash })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_covers_whitespace_because_whitespace_is_content() {
        // The reason the scheme exists at all is drift, and a formatter
        // stripping a trailing space is drift. A hash that trimmed first would
        // call the changed line unchanged and let the edit land on it.
        assert_ne!(hash_line("let x = 1;"), hash_line("let x = 1; "));
        assert_ne!(hash_line("let x = 1;"), hash_line("    let x = 1;"));
    }

    #[test]
    fn near_identical_lines_get_distinct_labels() {
        // Deliberately *not* a test of the fold — that claim was measured and
        // did not hold; see the module doc. What this does guard is the thing a
        // future "cheaper hash" would break: lines that differ only in the
        // middle must still get distinct labels, because those are what real
        // source looks like and a hash that ignored them would let an edit land
        // on a changed line. 2000 draws from a 65536-space collide about 30
        // times by birthday alone, so the floor allows for that and no more.
        let mut seen = std::collections::HashSet::new();
        for n in 0..2000 {
            seen.insert(short(hash_line(&format!("    let x = {n};"))));
        }
        assert!(
            seen.len() > 1940,
            "only {} distinct labels over 2000 near-identical lines",
            seen.len()
        );
    }

    #[test]
    fn a_bare_line_number_is_refused_by_name() {
        let err = parse("12").unwrap_err();
        assert!(err.contains("no #hash"), "{err}");
    }

    #[test]
    fn a_clipped_marker_is_refused_with_its_reason() {
        let err = parse("12#----").unwrap_err();
        assert!(err.contains("clip"), "{err}");
    }

    #[test]
    fn ranges_parse_and_must_run_forwards() {
        let a = parse("12#a3f9-15#b7c1").unwrap();
        assert_eq!(a.first(), 12);
        assert_eq!(a.last(), 15);
        assert_eq!(a.ends().len(), 2);

        let err = parse("15#b7c1-12#a3f9").unwrap_err();
        assert!(err.contains("ends before it starts"), "{err}");
    }

    #[test]
    fn case_is_normalised_on_the_way_in() {
        assert_eq!(parse("12#A3F9").unwrap().start.hash, "a3f9");
    }
}
