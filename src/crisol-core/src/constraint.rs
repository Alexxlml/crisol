//! Version constraints as projects write them: `^8.1`, `>=18 <22`, `~8.2.0`, `8.2.*`,
//! `^8.1 || ^8.2` in a `composer.json` (`require.php`) or a `package.json` (`engines.node`).
//!
//! Only what is needed to choose among the versions that are **installed**: whether a given
//! [`Version`] satisfies the constraint. The two ecosystems agree on almost everything and
//! differ in a few corners, so the parser takes a [`Flavor`]:
//!
//! | Written | Composer | npm |
//! |---|---|---|
//! | `~1.2` | `>=1.2.0 <2.0.0` | `>=1.2.0 <1.3.0` |
//! | `>1.2` | `>1.2.0` | `>=1.3.0` |
//! | `<=1.2` | `<=1.2.0` | `<1.3.0` |
//!
//! Both accept `||` (Composer also a single `|`), a space or a comma for "and", `x`, `X` and
//! `*` as wildcards, a `v` prefix, a `1.0 - 2.0` hyphen range, and an `@stability` suffix,
//! which is ignored. A bare partial version (`8.1`) means "any `8.1.x`", as in npm; this is
//! kind to people who write `"php": "8.1"`.
//!
//! A pre-release (`8.5.0-rc1`) never satisfies a range, in the spirit of both tools: a
//! release candidate is only used when someone asks for it by name.

use std::fmt;

use crate::version::Version;

/// Which tool's rules to follow where they differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `composer.json`.
    Composer,
    /// `package.json`.
    Npm,
}

/// A constraint that could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is not a version constraint: {reason}")]
pub struct ConstraintError {
    /// What was being parsed.
    pub text: String,
    /// What is wrong with it.
    pub reason: String,
}

/// One comparison against a version.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cmp {
    Ge(Version),
    Gt(Version),
    Le(Version),
    Lt(Version),
    Eq(Version),
    Ne(Version),
}

impl Cmp {
    fn matches(&self, version: &Version) -> bool {
        match self {
            Self::Ge(v) => version >= v,
            Self::Gt(v) => version > v,
            Self::Le(v) => version <= v,
            Self::Lt(v) => version < v,
            Self::Eq(v) => version == v,
            Self::Ne(v) => version != v,
        }
    }
}

/// A parsed constraint: any of several alternatives, each a list of comparisons that must
/// all hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    any_of: Vec<Vec<Cmp>>,
}

impl Constraint {
    /// Parses `text` following the rules of `flavor`.
    pub fn parse(text: &str, flavor: Flavor) -> Result<Self, ConstraintError> {
        let fail = |reason: String| ConstraintError {
            text: text.to_owned(),
            reason,
        };
        let mut any_of = Vec::new();
        for alternative in text.replace("||", "|").split('|') {
            let comparisons = parse_alternative(alternative, flavor).map_err(fail)?;
            any_of.push(comparisons);
        }
        Ok(Self { any_of })
    }

    /// Whether `version` satisfies the constraint. Pre-releases never do.
    #[must_use]
    pub fn matches(&self, version: &Version) -> bool {
        !version.is_prerelease()
            && self
                .any_of
                .iter()
                .any(|all| all.iter().all(|cmp| cmp.matches(version)))
    }
}

impl fmt::Display for Flavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Composer => "composer",
            Self::Npm => "npm",
        })
    }
}

/// One alternative: terms separated by blanks or commas, with `A - B` ranges.
fn parse_alternative(text: &str, flavor: Flavor) -> Result<Vec<Cmp>, String> {
    let cleaned = text.replace(',', " ");
    let mut tokens: Vec<String> = Vec::new();
    for token in cleaned.split_whitespace() {
        // `>= 8.1` is written with a blank after the operator by some people.
        if let Some(last) = tokens.last_mut() {
            if !last.is_empty() && last.chars().all(|c| "<>=!^~".contains(c)) {
                last.push_str(token);
                continue;
            }
        }
        tokens.push(token.to_owned());
    }

    let mut out = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if tokens.get(index + 1).map(String::as_str) == Some("-") {
            let from = tokens.get(index).ok_or("a range needs a start")?;
            let to = tokens.get(index + 2).ok_or("a range needs an end")?;
            out.extend(hyphen_range(from, to)?);
            index += 3;
        } else {
            out.extend(parse_term(&tokens[index], flavor)?);
            index += 1;
        }
    }
    Ok(out)
}

/// The numeric parts of a term, and whether it ended in a wildcard.
struct Parts {
    numbers: Vec<u64>,
}

fn parse_parts(original: &str) -> Result<Parts, String> {
    let text = original.split('@').next().unwrap_or_default();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    // A pre-release or build suffix on a bound does not change which release it names.
    let text = text.split(['-', '+']).next().unwrap_or_default();
    if text.is_empty() {
        // Nothing at all is "any version"; a lone `-` or `@dev` is a mistake.
        return if original.is_empty() {
            Ok(Parts {
                numbers: Vec::new(),
            })
        } else {
            Err(format!("`{original}` has no version"))
        };
    }
    let mut numbers = Vec::new();
    let mut wildcard = false;
    for part in text.split('.') {
        match part {
            "x" | "X" | "*" => wildcard = true,
            _ if wildcard => return Err(format!("`{part}` follows a wildcard")),
            digits if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
                numbers.push(
                    digits
                        .parse()
                        .map_err(|_| format!("`{part}` is too large"))?,
                );
            }
            other => return Err(format!("`{other}` is not a number")),
        }
    }
    Ok(Parts { numbers })
}

fn version_of(numbers: &[u64]) -> Version {
    let mut parts = numbers.to_vec();
    while parts.len() < 3 {
        parts.push(0);
    }
    let text = parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");
    // The text is made of numbers and dots only, so it always parses.
    Version::parse(&text).unwrap_or_else(|_| unreachable!("`{text}` is a valid version"))
}

/// `numbers` with the part at `index` raised by one and the rest dropped: the first
/// version that is no longer in the range (`[8, 1]` at 1 gives `8.2`).
fn bump(numbers: &[u64], index: usize) -> Version {
    let mut parts: Vec<u64> = numbers.iter().take(index + 1).copied().collect();
    if let Some(last) = parts.get_mut(index) {
        *last += 1;
    }
    version_of(&parts)
}

fn parse_term(token: &str, flavor: Flavor) -> Result<Vec<Cmp>, String> {
    let split = token
        .find(|c: char| !"<>=!^~".contains(c))
        .unwrap_or(token.len());
    let (operator, rest) = token.split_at(split);
    let numbers = parse_parts(rest)?.numbers;
    let n = numbers.len();
    let partial = n < 3;

    if n == 0 {
        if rest.is_empty() && !operator.is_empty() {
            return Err(format!("`{token}` has no version"));
        }
        return match operator {
            "" | "=" | ">=" | "^" | "~" => Ok(Vec::new()), // any version
            _ => Err(format!("`{token}` has no version")),
        };
    }
    let low = version_of(&numbers);
    Ok(match operator {
        "" | "=" if partial => vec![Cmp::Ge(low), Cmp::Lt(bump(&numbers, n - 1))],
        "" | "=" => vec![Cmp::Eq(low)],
        "^" => {
            // The first part that is not zero is the one that may not change.
            let index = numbers.iter().position(|p| *p != 0).unwrap_or(n - 1);
            vec![Cmp::Ge(low), Cmp::Lt(bump(&numbers, index))]
        }
        "~" => {
            let index = match flavor {
                Flavor::Npm => usize::from(n > 1),
                Flavor::Composer => n.saturating_sub(2),
            };
            vec![Cmp::Ge(low), Cmp::Lt(bump(&numbers, index))]
        }
        ">=" => vec![Cmp::Ge(low)],
        ">" if partial && flavor == Flavor::Npm => vec![Cmp::Ge(bump(&numbers, n - 1))],
        ">" => vec![Cmp::Gt(low)],
        "<" => vec![Cmp::Lt(low)],
        "<=" if partial && flavor == Flavor::Npm => vec![Cmp::Lt(bump(&numbers, n - 1))],
        "<=" => vec![Cmp::Le(low)],
        "!=" | "<>" if !partial => vec![Cmp::Ne(low)],
        "!=" | "<>" => return Err("`!=` needs a complete version".to_owned()),
        other => return Err(format!("unknown operator `{other}`")),
    })
}

/// `A - B`: from A up to B, where a partial B includes everything that starts with it.
fn hyphen_range(from: &str, to: &str) -> Result<Vec<Cmp>, String> {
    let low = parse_parts(from)?.numbers;
    let high = parse_parts(to)?.numbers;
    if low.is_empty() || high.is_empty() {
        return Err("a range needs two versions".to_owned());
    }
    let upper = if high.len() < 3 {
        Cmp::Lt(bump(&high, high.len() - 1))
    } else {
        Cmp::Le(version_of(&high))
    };
    Ok(vec![Cmp::Ge(version_of(&low)), upper])
}

#[cfg(test)]
mod tests;
