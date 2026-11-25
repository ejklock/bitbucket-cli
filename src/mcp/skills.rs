//! The bitbucket skill as the MCP skills extension serves it: one resource and one catalog
//! entry built from embedded text. No request argument ever reaches the filesystem.

use rmcp::model::{Resource, Skill, SkillFrontmatter, SkillResource, SkillResources};

const SKILL_TEXT: &str = include_str!("../../plugins/bitbucket/skills/bitbucket/SKILL.md");
const MARKDOWN: &str = "text/markdown";
const FRONTMATTER_FENCE: &str = "---";

/// The uri the skill is served at.
pub const SKILL_URI: &str = "skill://bitbucket/SKILL.md";

/// The one embedded skill: its text, its resource and its catalog entry.
#[derive(Default)]
pub(super) struct SkillCatalog {
    entry: Option<Entry>,
}

struct Entry {
    text: &'static str,
    resource: Resource,
    skill: Skill,
}

impl SkillCatalog {
    /// The catalog of the embedded skill, or why its frontmatter is unusable.
    pub(super) fn embedded() -> Result<Self, String> {
        Self::from_text(SKILL_TEXT)
    }

    fn from_text(text: &'static str) -> Result<Self, String> {
        let (name, description) = parse_frontmatter(text)?;
        let resource = Resource::new(SKILL_URI, &name)
            .with_description(&description)
            .with_mime_type(MARKDOWN);
        let files = vec![SkillResource::from_bytes(SKILL_URI, text.as_bytes())];
        let skill = Skill::new(
            SKILL_URI,
            SkillFrontmatter::new(name, description),
            SkillResources::Files(files),
        );
        Ok(Self {
            entry: Some(Entry {
                text,
                resource,
                skill,
            }),
        })
    }

    pub(super) fn resources(&self) -> Vec<Resource> {
        self.entry.iter().map(|e| e.resource.clone()).collect()
    }

    /// The text served at exactly `uri`.
    pub(super) fn read(&self, uri: &str) -> Option<&'static str> {
        self.entry
            .as_ref()
            .filter(|_| uri == SKILL_URI)
            .map(|e| e.text)
    }

    pub(super) fn skills(&self) -> Vec<Skill> {
        self.entry.iter().map(|e| e.skill.clone()).collect()
    }

    /// The entry served at exactly `uri`.
    pub(super) fn skill(&self, uri: &str) -> Option<&Skill> {
        self.entry
            .as_ref()
            .filter(|_| uri == SKILL_URI)
            .map(|e| &e.skill)
    }
}

/// `name` and `description` of the frontmatter, whose lines are plain `key: value` scalars.
fn parse_frontmatter(text: &str) -> Result<(String, String), String> {
    let mut lines = text.lines();
    if lines.next() != Some(FRONTMATTER_FENCE) {
        return Err("SKILL.md does not open with a frontmatter".to_owned());
    }
    let mut name = None;
    let mut description = None;
    let mut closed = false;
    for line in lines {
        if line == FRONTMATTER_FENCE {
            closed = true;
            break;
        }
        match line.split_once(": ") {
            Some(("name", value)) => name = Some(value.trim()),
            Some(("description", value)) => description = Some(value.trim()),
            _ => {}
        }
    }
    if !closed {
        return Err("SKILL.md frontmatter is not closed".to_owned());
    }
    let field = |key: &str, value: Option<&str>| {
        value
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("SKILL.md frontmatter needs a non-empty `{key}`"))
    };
    Ok((field("name", name)?, field("description", description)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_skill_is_served_at_its_uri_only() {
        let catalog = SkillCatalog::embedded().expect("the embedded frontmatter parses");
        assert_eq!(catalog.read(SKILL_URI), Some(SKILL_TEXT));
        assert_eq!(catalog.read("skill://bitbucket/other.md"), None);
        assert!(catalog.skill(SKILL_URI).is_some());
        assert!(catalog.skill("skill://bitbucket/").is_none());
        assert_eq!(catalog.resources().len(), 1);
        assert_eq!(catalog.skills().len(), 1);
    }

    #[test]
    fn a_frontmatter_without_the_opening_fence_is_refused() {
        let error = SkillCatalog::from_text("name: a\ndescription: b\n---\n").err();
        assert!(error.is_some_and(|e| e.contains("does not open")));
    }

    #[test]
    fn a_frontmatter_without_the_closing_fence_is_refused() {
        let error = SkillCatalog::from_text("---\nname: a\ndescription: b\n").err();
        assert!(error.is_some_and(|e| e.contains("not closed")));
    }

    #[test]
    fn a_frontmatter_missing_or_empty_name_or_description_is_refused() {
        for text in [
            "---\ndescription: b\n---\n",
            "---\nname: a\n---\n",
            "---\nname: \ndescription: b\n---\n",
            "---\nname: a\ndescription:\n---\n",
        ] {
            let error = SkillCatalog::from_text(text).err();
            assert!(error.is_some_and(|e| e.contains("non-empty")), "{text:?}");
        }
    }
}
