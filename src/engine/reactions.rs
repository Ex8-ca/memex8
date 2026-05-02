//! Reaction Tracking — Phase 1 of Proactive Memory Inference
//!
//! Analyzes text content to infer emotional/engagement reaction scores,
//! which are used to boost or dampen memory importance during decay.

/// Reaction score range — negative (negative engagement) to positive (positive engagement).
pub type ReactionScore = f32;

/// Infer a reaction score from text content.
/// Returns a value in range [-1.0, 1.0]:
///   - 1.0  → very positive ("great!", "awesome", "perfect", "thanks", "love it")
///   - 0.0  → neutral / passive ("ok", "sure", "whatever")
///   - -1.0 → very negative ("frustrated", "annoying", "broken", "hate", "terrible")
///
/// ## Signals
/// - **Positive**: strong approval words, gratitude, enthusiasm markers
/// - **Negative**: frustration words, complaints, problem indicators
/// - **Engagement**: detailed responses, questions, follow-ups suggest high engagement
/// - **Passive**: one-word answers, acknowledgments without elaboration
pub fn infer_reaction(text: &str) -> ReactionScore {
    let text_lower = text.to_lowercase();

    // ── Positive signals ──────────────────────────────────────────
    let positive_words = [
        "great", "awesome", "perfect", "thanks", "thank you", "love", "loved",
        "excellent", "amazing", "wonderful", "fantastic", "brilliant", "superb",
        "helpful", "impressive", "nice", "good", "best", "better", "happy",
        "glad", "pleased", "satisfied", "excited", "thrilled", "enjoyed",
        "wow", "cool", "sweet", "yes", "yeah", "yay", "works great",
        "solved", "fixed", "resolved", "success", "successful",
    ];

    // ── Negative signals ──────────────────────────────────────────
    let negative_words = [
        "frustrated", "frustrating", "annoying", "annoyed", "broken", "hate",
        "terrible", "awful", "horrible", "worst", "bad", "failed", "failure",
        "error", "bug", "issue", "problem", "not working", "doesn't work",
        "cant stand", "can't stand", "disappointed", "disappointing", "upset",
        "angry", "mad", "furious", "annoyed", "irritated", "sucks", "sucked",
        "difficult", "hard", "struggle", "struggling", "confused", "confusing",
    ];

    // ── Engagement signals (boost score magnitude) ───────────────
    let engagement_words = [
        "because", "since", "however", "but", "also", " moreover", "therefore",
        "explained", "described", "detailed", "basically", "essentially",
        "actually", "really", "very", "extremely", "incredibly",
        "question", "wondering", "curious", "want to know", "figured out",
        "discovered", "realized", "found that", "notice", "observed",
    ];

    // Passive indicators (dampen score)
    let passive_words = [
        "ok", "okay", "sure", "whatever", "alright", "fine", "i see",
        "noted", "understood", "acknowledged", "received", "k",
    ];

    let word_count = text.split_whitespace().count().max(1) as f32;

    // Count positive matches
    let pos_count = positive_words
        .iter()
        .filter(|w| text_lower.contains(*w))
        .count() as f32;

    // Count negative matches
    let neg_count = negative_words
        .iter()
        .filter(|w| text_lower.contains(*w))
        .count() as f32;

    // Count engagement matches
    let eng_count = engagement_words
        .iter()
        .filter(|w| text_lower.contains(*w))
        .count() as f32;

    // Count passive matches
    let pas_count = passive_words
        .iter()
        .filter(|w| text_lower.contains(*w))
        .count() as f32;

    // Base score from sentiment
    let sentiment = (pos_count - neg_count) / word_count.max(10.0);

    // Engagement multiplier — detailed content suggests higher engagement
    let eng_multiplier = 1.0 + (eng_count * 0.1);

    // Passive damping — passive responses reduce the magnitude
    let passive_dampen = 1.0 - (pas_count * 0.2).min(0.5);

    // Compute final score
    let score = sentiment * 5.0 * eng_multiplier * passive_dampen;

    // Clamp to [-1.0, 1.0]
    score.clamp(-1.0, 1.0)
}

/// Compute the reaction boost factor used in decay_memories.
///
/// `reaction_boost = 1.0 + (reaction_score * 0.3)`
/// Range: 0.7 (very negative) to 1.3 (very positive)
#[inline]
pub fn reaction_boost(reaction_score: ReactionScore) -> f32 {
    1.0 + (reaction_score * 0.3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_positive_reaction() {
        let score = infer_reaction("This is great! Thank you so much, awesome work!");
        assert!(score > 0.5, "Expected positive score, got {}", score);
    }

    #[test]
    fn test_negative_reaction() {
        let score = infer_reaction("This is broken and frustrating. I hate this bug.");
        assert!(score < -0.3, "Expected negative score, got {}", score);
    }

    #[test]
    fn test_neutral_reaction() {
        let score = infer_reaction("Ok, fine, whatever.");
        assert!(score.abs() < 0.3, "Expected near-zero score, got {}", score);
    }

    #[test]
    fn test_engagement_boost() {
        // Detailed response should be scored higher than passive acknowledgment
        let detailed = "I solved the issue because I realized the config was wrong. The server worked after I fixed the port.";
        let simple = "ok";
        assert!(
            infer_reaction(detailed) > infer_reaction(simple),
            "Detailed response should score higher"
        );
    }

    #[test]
    fn test_reaction_boost_range() {
        assert!((reaction_boost(1.0) - 1.3).abs() < 0.001);
        assert!((reaction_boost(-1.0) - 0.7).abs() < 0.001);
        assert!((reaction_boost(0.0) - 1.0).abs() < 0.001);
    }
}
