use std::collections::BTreeMap;

use nixon::{
  Element,
  SyntaxKind,
  ast::{
    AstNode,
    AttributeComponent,
    AttributeEntry,
    AttributeSet,
    Binding,
    Expression,
    Root,
  },
};
use serde::Serialize;

pub type AttrPath = Vec<String>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize)]
pub struct IgnorePattern {
  pub path:   AttrPath,
  pub offset: usize,
}

#[derive(Debug, Default)]
pub struct FlakeNix {
  pub input_ignores: BTreeMap<String, Vec<IgnorePattern>>,
  pub src:           String,
}

impl FlakeNix {
  pub fn from_str(src: &str) -> Result<Self, String> {
    let doc = nixon::parse(src)
      .map_err(|e| format!("failed to parse flake.nix: {e}"))?;

    let top_level = Root::cast(doc.root())
      .and_then(Root::expression)
      .and_then(unwrap_attrset)
      .ok_or_else(|| String::from("top-level must be an attribute set"))?;

    let input_bindings: Vec<Binding<'_, '_>> = top_level
      .entries()
      .filter_map(as_binding)
      .filter(|&b| get_binding_name(b) == Some("inputs"))
      .collect();
    if input_bindings.is_empty() {
      return Err(String::from("missing required `inputs` attribute"));
    }

    let input_ignores =
      input_bindings
        .into_iter()
        .fold(BTreeMap::new(), |mut acc, binding| {
          merge_ignores(&mut acc, absolute_ignores(binding));
          merge_ignores(&mut acc, relative_ignores(binding));
          acc
        });

    Ok(Self {
      input_ignores,
      src: src.to_owned(),
    })
  }

  pub fn find_ignore<'a, 'b>(
    &'a self,
    path: &'b [String],
  ) -> Option<(&'b str, &'a IgnorePattern)> {
    let (root, tail) = path.split_first()?;
    let patterns = self.input_ignores.get(root)?;

    patterns
      .iter()
      .find(|IgnorePattern { path, .. }| tail.starts_with(path))
      .map(|pattern| (root.as_str(), pattern))
  }

  pub fn should_ignore(&self, path: &[String]) -> bool {
    self.find_ignore(path).is_some()
  }
}

fn merge_ignores(
  acc: &mut BTreeMap<String, Vec<IgnorePattern>>,
  other: BTreeMap<String, Vec<IgnorePattern>>,
) {
  for (root, pattern) in other {
    acc.entry(root).or_default().extend(pattern);
  }
}

fn collect_ignores<F>(
  binding: Binding<'_, '_>,
  map_pattern: F,
) -> BTreeMap<String, Vec<IgnorePattern>>
where
  F: Fn(AttrPath) -> Option<(String, AttrPath)>,
{
  parse_ignores(binding)
    .into_iter()
    .filter_map(|(path, offset)| {
      map_pattern(path)
        .map(|(root, path)| (root, IgnorePattern { path, offset }))
    })
    .fold(BTreeMap::new(), |mut map, (root, pattern)| {
      map.entry(root).or_default().push(pattern);
      map
    })
}

fn absolute_ignores(
  binding: Binding<'_, '_>,
) -> BTreeMap<String, Vec<IgnorePattern>> {
  collect_ignores(binding, |path| {
    path
      .split_first()
      .map(|(root, tail)| (root.to_owned(), tail.to_vec()))
  })
}

fn relative_ignores(
  binding: Binding<'_, '_>,
) -> BTreeMap<String, Vec<IgnorePattern>> {
  let is_dotted = binding
    .path()
    .is_some_and(|path| path.components().count() > 1);
  if is_dotted {
    return BTreeMap::new();
  }

  let Some(inputs_attrset) = binding.value().and_then(unwrap_attrset) else {
    return BTreeMap::new();
  };

  inputs_attrset
    .entries()
    .filter_map(as_binding)
    .filter_map(|entry| {
      let name = get_binding_name(entry)?;
      Some((entry, name))
    })
    .fold(BTreeMap::new(), |mut acc, (entry, name)| {
      merge_ignores(
        &mut acc,
        collect_ignores(entry, |path| Some((name.to_owned(), path))),
      );
      acc
    })
}

fn unwrap_attrset<'doc, 'src>(
  expr: Expression<'doc, 'src>,
) -> Option<AttributeSet<'doc, 'src>> {
  match expr {
    Expression::AttributeSet(set) => Some(set),
    Expression::LetIn(let_in) => unwrap_attrset(let_in.body()?),
    Expression::With(with) => unwrap_attrset(with.body()?),
    _ => None,
  }
}

const fn as_binding<'doc, 'src>(
  entry: AttributeEntry<'doc, 'src>,
) -> Option<Binding<'doc, 'src>> {
  match entry {
    AttributeEntry::Binding(binding) => Some(binding),
    AttributeEntry::Inherit(_) => None,
  }
}

fn get_binding_name<'src>(binding: Binding<'_, 'src>) -> Option<&'src str> {
  match binding.path()?.components().next()? {
    AttributeComponent::Identifier(token) => Some(token.text()),
    _ => None,
  }
}

fn parse_ignores(binding: Binding<'_, '_>) -> Vec<(AttrPath, usize)> {
  let Some(path) = binding.path() else {
    return Vec::new();
  };

  path
    .syntax()
    .children()
    .map_while(|element| {
      match element {
        Element::Token(token) => Some(token),
        Element::Node(_) => None,
      }
    })
    .take_while(|token| {
      token.kind().is_trivia()
        || matches!(
          token.kind(),
          SyntaxKind::LineComment | SyntaxKind::BlockComment
        )
    })
    .filter(|token| {
      matches!(
        token.kind(),
        SyntaxKind::LineComment | SyntaxKind::BlockComment
      )
    })
    .flat_map(|token| {
      let token_start = token.range().start().as_usize();
      parse_ignore_patterns(token.text())
        .into_iter()
        .map(move |(rel_offset, path)| (path, token_start + rel_offset))
    })
    .collect()
}

fn parse_ignore_patterns(comment: &str) -> Vec<(usize, AttrPath)> {
  const MARKER: &str = "flint-ignore:";

  let Some((before, after)) = comment.split_once(MARKER) else {
    return Vec::new();
  };

  if !before.trim_end().ends_with('#') && !before.trim_end().ends_with("/*") {
    return Vec::new();
  }

  let after_offset = before.len() + MARKER.len();
  let body = after.strip_suffix("*/").unwrap_or(after);

  split_whitespace_with_offsets(body)
    .filter_map(|(offset, word)| {
      let path: AttrPath = word
        .split('/')
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect();
      (!path.is_empty()).then_some((after_offset + offset, path))
    })
    .collect()
}

fn split_whitespace_with_offsets(
  s: &str,
) -> impl Iterator<Item = (usize, &str)> {
  let base = s.as_ptr() as usize;
  s.split_whitespace()
    .map(move |word| (word.as_ptr() as usize - base, word))
}

#[cfg(test)]
mod tests {
  use super::FlakeNix;

  macro_rules! assert_ignore {
    ($flake:expr, [$($part:expr),* $(,)?]) => {
        assert!($flake.should_ignore(&[$($part.to_string()),*]));
    };
  }
  macro_rules! assert_not_ignore {
    ($flake:expr, [$($part:expr),* $(,)?]) => {
        assert!(!$flake.should_ignore(&[$($part.to_string()),*]));
    };
  }

  #[test]
  fn input_comments() {
    let src = r#"
      {
        # flint-ignore: beta
        inputs = {
          alpha.url = "...";
          beta.url = "...";

          /* flint-ignore: alpha epsilon/zeta */
          gamma = {};

          # flint-ignore: eta/theta
          delta = {};
        };
      }
    "#;
    let flake = FlakeNix::from_str(src).unwrap();

    assert_ignore!(flake, ["beta"]);
    assert_not_ignore!(flake, ["alpha"]);

    assert_ignore!(flake, ["gamma", "alpha"]);
    assert_ignore!(flake, ["gamma", "epsilon", "zeta"]);
    assert_not_ignore!(flake, ["gamma", "beta"]);
    assert_not_ignore!(flake, ["gamma", "eta"]);

    assert_ignore!(flake, ["delta", "eta", "theta"]);
    assert_not_ignore!(flake, ["delta", "eta"]);
  }

  #[test]
  fn let_with_unwrapping() {
    let src = "
      let x.y = 67; in
      with x;
      let z = y; in
      {
        inputs = {
          # flint-ignore: beta
          alpha = {};
        };
      }
    ";
    let flake = FlakeNix::from_str(src).unwrap();

    assert_ignore!(flake, ["alpha", "beta"]);
    assert_not_ignore!(flake, ["alpha", "gamma"]);
  }

  #[test]
  fn dotted_inputs() {
    let src = r#"
      {
        inputs.alpha.url = "...";

        # flint-ignore: beta
        inputs.beta.url = "...";

        # flint-ignore: gamma/delta
        inputs.gamma.url = "...";
      }
    "#;
    let flake = FlakeNix::from_str(src).unwrap();

    assert_ignore!(flake, ["beta"]);
    assert_not_ignore!(flake, ["alpha"]);

    assert_ignore!(flake, ["gamma", "delta"]);
    assert_not_ignore!(flake, ["gamma"]);
  }

  #[test]
  fn mixed_input_styles() {
    let src = r#"
      {
        # flint-ignore: beta
        inputs.alpha.url = "...";
        inputs.beta.url = "...";

        inputs = {
          # flint-ignore: delta
          gamma.url = "...";
        };
      }
    "#;
    let flake = FlakeNix::from_str(src).unwrap();

    assert_ignore!(flake, ["beta"]);
    assert_not_ignore!(flake, ["alpha"]);

    assert_ignore!(flake, ["gamma", "delta"]);
    assert_not_ignore!(flake, ["gamma", "epsilon"]);
  }
}
