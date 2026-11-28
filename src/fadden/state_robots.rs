// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! robots.txt rules as `fadden state` applies them.

/// The robots.txt token `state::AGENT` answers to.
const AGENT_TOKEN: &str = "au-legislation-corpus";

/// The rules of the robots.txt groups that apply to `AGENT_TOKEN`: its own
/// group when one names it, otherwise `*`.
#[derive(Debug, Default)]
pub struct Robots {
    rules: Vec<(bool, String)>,
    pub crawl_delay: Option<f64>,
}

/// One `User-agent` group: its agents, rules and crawl delay.
struct Group {
    agents: Vec<String>,
    rules: Vec<(bool, String)>,
    delay: Option<f64>,
}

impl Group {
    /// Whether a non-empty agent other than `*` is part of `AGENT_TOKEN`. An
    /// empty `User-agent:` value names no robot, though every string contains "".
    fn named(&self) -> bool {
        self.agents
            .iter()
            .any(|a| !a.is_empty() && a != "*" && AGENT_TOKEN.contains(a.as_str()))
    }
}

/// robots.txt's groups in order: consecutive `User-agent` lines open one, and
/// the lines after them until the next `User-agent` belong to it.
fn groups(text: &str) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let (field, value) = (field.trim().to_ascii_lowercase(), value.trim());
        if field == "user-agent" {
            if !open {
                groups.push(Group {
                    agents: Vec::new(),
                    rules: Vec::new(),
                    delay: None,
                });
                open = true;
            }
            if let Some(group) = groups.last_mut() {
                group.agents.push(value.to_ascii_lowercase());
            }
            continue;
        }
        open = false;
        let Some(group) = groups.last_mut() else {
            continue;
        };
        match field.as_str() {
            "allow" => group.rules.push((true, value.to_string())),
            "disallow" => group.rules.push((false, value.to_string())),
            "crawl-delay" => group.delay = value.parse().ok(),
            _ => {}
        }
    }
    groups
}

impl Robots {
    pub fn parse(text: &str) -> Robots {
        let groups = groups(text);
        let any_named = groups.iter().any(Group::named);
        let mut robots = Robots::default();
        for group in &groups {
            let applies = if any_named {
                group.named()
            } else {
                group.agents.iter().any(|a| a == "*")
            };
            if applies {
                robots.rules.extend(group.rules.iter().cloned());
                if let Some(delay) = group.delay {
                    robots.crawl_delay = Some(robots.crawl_delay.unwrap_or(0.0).max(delay));
                }
            }
        }
        robots
    }

    /// Whether `path` may be fetched: the longest matching rule wins, Allow on a
    /// tie; an empty Disallow allows everything.
    pub fn allows(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if pattern.is_empty() || !matches(pattern, path) {
                continue;
            }
            let better = match best {
                None => true,
                Some((length, verdict)) => {
                    pattern.len() > length || (pattern.len() == length && *allow && !verdict)
                }
            };
            if better {
                best = Some((pattern.len(), *allow));
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// A robots.txt path pattern: a prefix, with `*` for any run and a final `$`
/// for the end.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(rest) => (rest, true),
        None => (pattern, false),
    };
    let mut pieces = pattern.split('*');
    let first = pieces.next().unwrap_or("");
    let Some(mut rest) = path.strip_prefix(first) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    for (index, piece) in pieces.iter().enumerate() {
        let last = index + 1 == pieces.len();
        if last && anchored {
            return rest.ends_with(piece);
        }
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    !anchored || rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_parser_and_wildcard_edge_cases() {
        let rules = Robots::parse(
            "Disallow: /orphan\nUser-agent: *\nSitemap: https://x/\nDisallow: /*.pdf$\nDisallow: /a*b*c\n",
        );
        assert!(!rules.allows("/x/doc.pdf"));
        assert!(rules.allows("/x/doc.pdfx"));
        assert!(!rules.allows("/a1b2c"));
        assert!(rules.allows("/a1d"));
    }
}
