//! A part of a value: an attribute and a path from its value to one
//! member of an object or one element of a list.

use std::fmt::Write as _;

use serde_json::Value;

use crate::claim::Attribute;
use crate::error::{Error, Result};

/// One step of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// A key of an object.
    Key(String),
    /// The position of an element in a list, counted from 0.
    Index(usize),
}

/// An attribute with a path into its values, written as the attribute
/// followed by steps, each after a dot: `geo:position.latitude`,
/// `exif:gps-latitude.0`, `a:b.c.0`.
///
/// A step of digits is the position of an element in a list, counted
/// from 0. Any other step is a key of an object, in the characters an
/// attribute name allows: lower-case letters, digits and dashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    spelling: String,
    attribute: Attribute,
    steps: Vec<Step>,
}

impl Part {
    /// Parse `attribute.step.step…`.
    ///
    /// # Errors
    ///
    /// [`Error::Attribute`] when the text before the first dot is not an
    /// attribute; [`Error::Part`] when there is no step, or a step is
    /// empty or holds other characters.
    pub fn parse(s: &str) -> Result<Self> {
        let Some((name, path)) = s.split_once('.') else {
            return Err(Error::Part(s.to_string()));
        };
        let attribute = Attribute::parse(name)?;
        let steps = path
            .split('.')
            .map(step)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::Part(s.to_string()))?;
        Ok(Self {
            spelling: s.to_string(),
            attribute,
            steps,
        })
    }

    /// The part as it was written, attribute and steps.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.spelling
    }

    /// The attribute whose values the path leads into.
    #[must_use]
    pub fn attribute(&self) -> &Attribute {
        &self.attribute
    }

    /// The path in `SQLite`'s JSON path syntax, every key in quotes:
    /// `$."latitude"`, `$[0]`, `$."c"[0]`.
    #[must_use]
    pub fn json_path(&self) -> String {
        let mut path = String::from("$");
        for step in &self.steps {
            // Writing to a String cannot fail.
            let _ = match step {
                Step::Key(key) => write!(path, ".\"{key}\""),
                Step::Index(index) => write!(path, "[{index}]"),
            };
        }
        path
    }

    /// The part of `value`, or `None` when the value does not have it:
    /// a key that is missing, a position past the end, a key asked of a
    /// list or a position asked of an object.
    #[must_use]
    pub fn of<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        self.steps.iter().try_fold(value, |inner, step| match step {
            Step::Key(key) => inner.as_object()?.get(key),
            Step::Index(index) => inner.as_array()?.get(*index),
        })
    }
}

fn step(text: &str) -> Option<Step> {
    if text.is_empty() {
        return None;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse().ok().map(Step::Index);
    }
    text.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        .then(|| Step::Key(text.to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_part_is_an_attribute_and_its_steps() {
        let part = Part::parse("geo:position.latitude").unwrap();
        assert_eq!(part.as_str(), "geo:position.latitude");
        assert_eq!(part.attribute().as_str(), "geo:position");
        assert_eq!(part.json_path(), r#"$."latitude""#);

        assert_eq!(
            Part::parse("exif:gps-latitude.0").unwrap().json_path(),
            "$[0]"
        );
        assert_eq!(
            Part::parse("a:b.c-d.12").unwrap().json_path(),
            r#"$."c-d"[12]"#
        );
        assert_eq!(
            Part::parse("a:b.2nd").unwrap().json_path(),
            r#"$."2nd""#,
            "a step with letters is a key, even when it begins with a digit"
        );
    }

    #[test]
    fn a_part_outside_the_grammar_is_refused() {
        for wrong in [
            "geo:position",
            "geo:position.",
            "geo:position..latitude",
            "geo:position.Latitude",
            "geo:position.lat itude",
            "geo:position.lat\"",
        ] {
            assert!(
                matches!(Part::parse(wrong), Err(Error::Part(given)) if given == wrong),
                "{wrong:?}"
            );
        }
        assert!(
            matches!(Part::parse("Geo:position.latitude"), Err(Error::Attribute(given)) if given == "Geo:position"),
            "the attribute is named by its own grammar"
        );
    }

    #[test]
    fn a_part_is_taken_from_a_value_that_has_it() {
        let value = json!({"latitude": 48.137_154, "corners": [[1, 2], [3, 4]]});
        let at = |path: &str| Part::parse(&format!("a:b.{path}")).unwrap();

        assert_eq!(at("latitude").of(&value), Some(&json!(48.137_154)));
        assert_eq!(at("corners.1.0").of(&value), Some(&json!(3)));
        assert_eq!(at("altitude").of(&value), None, "a missing key");
        assert_eq!(at("corners.2").of(&value), None, "past the end of a list");
        assert_eq!(at("0").of(&value), None, "a position asked of an object");
        assert_eq!(at("corners.x").of(&value), None, "a key asked of a list");
        assert_eq!(at("latitude.0").of(&value), None, "a step into a number");
    }
}
