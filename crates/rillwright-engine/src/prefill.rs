//! How a prompt is split into `llama_decode` calls (the prefill schedule).
//!
//! For this model the split changes the logits, not just the rounding: a 275-token prompt decoded
//! as one chunk and as 271 + 4 gave different logits and, on some prompts, different greedy text.
//! `llama-server` splits prompts so that it can save checkpoints, so the harness does the same by
//! default. The rule below mirrors `tools/server/server-context.cpp` (prompt batch filling, around
//! lines 4480-4600 of the llama.cpp-dory fork) for a fresh slot with checkpoints on, which is the
//! server's default. See RUST_HARNESS_FIX_PLAN.md, Fix 1.

use std::str::FromStr;

/// The server's minimum spacing between checkpoints (`checkpoint_min_step`, default 8192).
const CHECKPOINT_MIN_STEP: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PrefillSchedule {
    /// llama-server's schedule with checkpoints on (its default). A chunk ends `4 + n_ubatch` and
    /// `4` tokens before the end of the prompt, and at the start of a user message where the
    /// server would break for a checkpoint (chat turns only; see [`message_starts`]).
    #[default]
    Server,
    /// The whole remainder in `n_batch`-sized chunks.
    Single,
}

impl FromStr for PrefillSchedule {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "server" => Ok(PrefillSchedule::Server),
            "single" => Ok(PrefillSchedule::Single),
            other => Err(format!(
                "unknown prefill schedule '{other}' (use server or single)"
            )),
        }
    }
}

/// Sizes of the chunks that decode prompt tokens `from..n`, in order.
///
/// `message_starts` are the token positions where a chat message starts (only user messages
/// matter here). Pass an empty slice for a raw prompt, as `/completion` does.
///
/// The result is exact for a fresh slot, that is, when `from` is 0 or the earlier tokens are
/// not already in the cache with server checkpoints. With prefix reuse on, the server's
/// checkpoint state can differ, so the chunks may not match it.
pub fn chunk_sizes(
    schedule: PrefillSchedule,
    from: usize,
    n: usize,
    n_batch: usize,
    n_ubatch: usize,
    message_starts: &[usize],
) -> Vec<usize> {
    let n_batch = n_batch.max(1);
    match schedule {
        PrefillSchedule::Single => {
            let mut sizes = Vec::new();
            let mut left = n.saturating_sub(from);
            while left > 0 {
                let take = left.min(n_batch);
                sizes.push(take);
                left -= take;
            }
            sizes
        }
        PrefillSchedule::Server => server_sizes(from, n, n_batch, n_ubatch, message_starts),
    }
}

fn server_sizes(
    from: usize,
    n: usize,
    n_batch: usize,
    n_ubatch: usize,
    message_starts: &[usize],
) -> Vec<usize> {
    // Break when this many tokens remain after the chunk (the server's n_last values).
    let offsets = [(4 + n_ubatch).min(n_batch), 4usize.min(n_batch)];
    let last_user = message_starts.last().copied();
    let mut checkpoints: Vec<usize> = Vec::new();
    let mut sizes = Vec::new();
    let mut start = from;

    while start < n {
        // Fill the chunk one token at a time, breaking where the server breaks.
        let mut end = start;
        while end < n && end - start < n_batch {
            end += 1;
            let user_start = message_starts.contains(&end);
            let spaced = checkpoints
                .last()
                .is_none_or(|&c| end > c + CHECKPOINT_MIN_STEP);
            if user_start && (Some(end) == last_user || checkpoints.is_empty() || spaced) {
                break;
            }
            if offsets.contains(&(n - end)) {
                break;
            }
        }

        // Checkpoint bookkeeping after the chunk, as the server does before decoding it. It decides
        // which later chunks break at user messages.
        let near_prompt_end = n < end + n_ubatch;
        let user_at_start = message_starts.contains(&start);
        let mut want_checkpoint = true;
        if end < n && !user_at_start && !near_prompt_end {
            want_checkpoint = false;
        }
        // Nothing is in the cache yet at position 0, so the server cannot checkpoint there.
        if want_checkpoint && start > 0 {
            let spaced = checkpoints
                .last()
                .is_none_or(|&c| start > c + CHECKPOINT_MIN_STEP);
            if checkpoints.is_empty() || Some(start) == last_user || near_prompt_end || spaced {
                checkpoints.push(start);
            }
        }

        sizes.push(end - start);
        start = end;
    }
    sizes
}

/// Token positions where a user turn starts: every place the tokenized user-turn marker occurs.
/// This is `server_tokens::find_message_spans` in the fork, which matches the template's
/// delimiter tokens in the token stream rather than in the text.
pub fn message_starts(tokens: &[i32], user_turn: &[i32]) -> Vec<usize> {
    if user_turn.is_empty() || user_turn.len() > tokens.len() {
        return Vec::new();
    }
    (0..=tokens.len() - user_turn.len())
        .filter(|&i| tokens[i..i + user_turn.len()] == *user_turn)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATCH: usize = 2048;
    const UBATCH: usize = 512;

    #[test]
    fn finds_user_turn_positions() {
        let tokens = [1, 2, 3, 9, 2, 3, 5, 2, 3];
        assert_eq!(message_starts(&tokens, &[2, 3]), vec![1, 4, 7]);
        assert!(message_starts(&tokens, &[7]).is_empty());
        assert!(message_starts(&tokens, &[]).is_empty());
        assert!(message_starts(&[2], &[2, 3]).is_empty());
    }

    fn server(n: usize) -> Vec<usize> {
        chunk_sizes(PrefillSchedule::Server, 0, n, BATCH, UBATCH, &[])
    }

    #[test]
    fn server_schedule_for_known_lengths() {
        assert_eq!(server(5), vec![1, 4]);
        assert_eq!(server(28), vec![24, 4]);
        assert_eq!(server(275), vec![271, 4]);
        assert_eq!(server(1000), vec![484, 512, 4]);
        assert_eq!(server(5000), vec![2048, 2048, 388, 512, 4]);
    }

    #[test]
    fn sizes_cover_the_prompt_exactly() {
        for n in [1usize, 4, 5, 516, 517, 2048, 2049, 4100, 9999] {
            let s = server(n);
            assert_eq!(s.iter().sum::<usize>(), n, "n={n}");
            assert!(s.iter().all(|&c| c >= 1 && c <= BATCH), "n={n}: {s:?}");
        }
    }

    #[test]
    fn single_schedule_uses_full_batches() {
        let s = chunk_sizes(PrefillSchedule::Single, 0, 5000, BATCH, UBATCH, &[]);
        assert_eq!(s, vec![2048, 2048, 904]);
        assert_eq!(
            chunk_sizes(PrefillSchedule::Single, 0, 275, BATCH, UBATCH, &[]),
            vec![275]
        );
    }

    #[test]
    fn prefix_offset_only_counts_the_new_tokens() {
        // 484 tokens already cached: the remaining 516 are split as 512 + 4.
        assert_eq!(
            chunk_sizes(PrefillSchedule::Server, 484, 1000, BATCH, UBATCH, &[]),
            vec![512, 4]
        );
        assert!(chunk_sizes(PrefillSchedule::Server, 1000, 1000, BATCH, UBATCH, &[]).is_empty());
    }

    #[test]
    fn chat_turn_breaks_at_the_user_message() {
        // A user message starts at token 10 of a 28-token prompt: break there, then the usual tail.
        assert_eq!(
            chunk_sizes(PrefillSchedule::Server, 0, 28, BATCH, UBATCH, &[10]),
            vec![10, 14, 4]
        );
    }

    #[test]
    fn small_batches_clamp_the_offsets() {
        // With n_batch = 8 the offsets are capped at 8, so every chunk is at most 8 tokens.
        let s = chunk_sizes(PrefillSchedule::Server, 0, 20, 8, UBATCH, &[]);
        assert_eq!(s.iter().sum::<usize>(), 20);
        assert!(s.iter().all(|&c| c <= 8), "{s:?}");
    }

    #[test]
    fn parses_schedule_names() {
        assert_eq!(
            "server".parse::<PrefillSchedule>(),
            Ok(PrefillSchedule::Server)
        );
        assert_eq!(
            "single".parse::<PrefillSchedule>(),
            Ok(PrefillSchedule::Single)
        );
        assert!("other".parse::<PrefillSchedule>().is_err());
    }
}
