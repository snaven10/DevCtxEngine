//! What an import does with each incoming memory.
//!
//! Import never overwrites and never deletes: its input comes from somewhere
//! else, so nothing already here may be lost by running it — including running
//! it with the wrong file. That is deliberately *not* `remember`'s rule, which
//! revises on a topic-key match and replaces the content. Correct when you are
//! amending your own note; destructive when the text arrived from another
//! machine, where it would silently replace a memory its sender never saw.

use devctx_store::Memory;

/// What an import decided about one incoming memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Not here; stored as-is.
    Added,
    /// The same content is already here; skipped.
    AlreadyPresent,
    /// Its topic key belongs to a different memory here. Stored anyway, without
    /// the key, so neither text is lost.
    TopicCollision,
}

/// Decide what to do with `incoming`, given everything already in the target
/// scope.
///
/// Decided before writing rather than while writing: an import that discovered
/// a conflict half-way through would already have replaced something.
pub fn decide(incoming: &Memory, existing: &[Memory]) -> Outcome {
    // Content identity, not id: two machines that recorded the same fact give
    // it different ids, and importing both should converge on one row.
    if existing
        .iter()
        .any(|e| e.normalized_hash == incoming.normalized_hash)
    {
        return Outcome::AlreadyPresent;
    }
    // Most memories carry no topic key. An empty one is the absence of a claim,
    // not a claim they all share — treating it as a match would collapse every
    // untopicked memory into a single collision.
    if !incoming.topic_key.is_empty() && existing.iter().any(|e| e.topic_key == incoming.topic_key)
    {
        return Outcome::TopicCollision;
    }
    Outcome::Added
}

/// The row to store, given the decision. Only a collision changes anything.
pub fn prepare(incoming: &Memory, outcome: Outcome) -> Memory {
    let mut m = incoming.clone();
    if outcome == Outcome::TopicCollision {
        // The local memory keeps the topic, so `remember --topic X` goes on
        // revising the one its author has been revising.
        m.topic_key = String::new();
    }
    m
}

/// What an import did, for the summary it prints.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub added: usize,
    pub already: usize,
    /// Titles of the memories kept alongside an existing topic owner. Titles,
    /// not a count: a collision is usually two people having learned different
    /// things about one subject, which is worth reading rather than resolving
    /// automatically.
    pub collisions: Vec<String>,
}

impl ImportReport {
    pub fn record(&mut self, m: &Memory, outcome: Outcome) {
        match outcome {
            Outcome::Added => self.added += 1,
            Outcome::AlreadyPresent => self.already += 1,
            Outcome::TopicCollision => {
                self.added += 1;
                self.collisions.push(if m.title.is_empty() {
                    m.id.clone()
                } else {
                    m.title.clone()
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: &str, topic: &str, content: &str, hash: &str) -> Memory {
        Memory {
            id: id.into(),
            title: id.into(),
            content: content.into(),
            topic_key: topic.into(),
            normalized_hash: hash.into(),
            project: "@group:REVFA".into(),
            ..Default::default()
        }
    }

    /// Nothing here yet: take it.
    #[test]
    fn an_unseen_memory_is_added() {
        let existing = vec![mem("mem_a", "auth", "a", "h1")];
        let incoming = mem("mem_b", "pdf", "b", "h2");
        assert_eq!(decide(&incoming, &existing), Outcome::Added);
    }

    /// The same content twice is the same memory, whatever its id says: two
    /// machines that both recorded one fact should converge, so importing a
    /// file twice must not double it.
    #[test]
    fn identical_content_is_recognised_however_it_is_labelled() {
        let existing = vec![mem("mem_a", "auth", "same text", "h1")];
        let incoming = mem("DIFFERENT_ID", "auth", "same text", "h1");
        assert_eq!(decide(&incoming, &existing), Outcome::AlreadyPresent);
    }

    /// The case that earns the rule. Two machines learned different things
    /// about one subject. Overwriting loses the local one; skipping loses the
    /// incoming one; keeping both loses neither.
    #[test]
    fn a_topic_collision_keeps_both() {
        let existing = vec![mem("mem_a", "auth", "we use JWT", "h1")];
        let incoming = mem("mem_b", "auth", "we moved to sessions", "h2");
        assert_eq!(decide(&incoming, &existing), Outcome::TopicCollision);
    }

    /// A collision must not leave two memories fighting over one topic: the
    /// incoming copy gives the key up, so the existing memory stays the one
    /// `remember --topic auth` will revise next time.
    #[test]
    fn the_incoming_copy_of_a_collision_gives_up_the_topic_key() {
        let incoming = mem("mem_b", "auth", "we moved to sessions", "h2");
        let stored = prepare(&incoming, Outcome::TopicCollision);
        assert_eq!(stored.topic_key, "", "the local memory keeps the topic");
        assert_eq!(
            stored.content, "we moved to sessions",
            "and nothing is lost"
        );
    }

    /// An empty topic key is not a collision — most memories have none, and
    /// treating "" as a shared topic would collapse them all into one.
    #[test]
    fn memories_without_a_topic_never_collide() {
        let existing = vec![mem("mem_a", "", "a", "h1")];
        let incoming = mem("mem_b", "", "b", "h2");
        assert_eq!(decide(&incoming, &existing), Outcome::Added);
    }

    /// A memory that arrives without a title is still named in the summary, by
    /// id — the list exists so a collision can be looked up afterwards, and an
    /// empty line cannot be looked up.
    #[test]
    fn a_collision_without_a_title_is_named_by_id() {
        let mut m = mem("mem_x", "auth", "text", "h9");
        m.title = String::new();
        let mut report = ImportReport::default();
        report.record(&m, Outcome::TopicCollision);
        assert_eq!(report.collisions, vec!["mem_x".to_string()]);
        assert_eq!(report.added, 1, "a collision is still stored");
    }
}
