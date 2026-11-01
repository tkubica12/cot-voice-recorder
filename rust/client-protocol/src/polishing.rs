use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::{ApiError, DictationRefinement};
use tokio::sync::{Notify, watch};

pub trait RefinementService: Send + Sync + 'static {
    fn refine(
        &self,
        text: &str,
        context: &str,
    ) -> impl Future<Output = Result<DictationRefinement, ApiError>> + Send;
}

#[derive(Clone)]
pub struct PolishProgress {
    pub text: String,
    pub raw_text: String,
    pub pending: bool,
    pub fallback_blocks: usize,
    pub successful_blocks: usize,
    pub unprocessed_words: usize,
    pub last_failure: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Clone)]
struct Call {
    start: usize,
    end: usize,
    raw: String,
    context: String,
    overlap: usize,
    final_call: bool,
}

struct State {
    raw: String,
    patches: Vec<Patch>,
    coverage: Vec<(usize, usize)>,
    arrivals: VecDeque<(usize, tokio::time::Instant)>,
    frontier: usize,
    submitted: usize,
    too_old: usize,
    pending_since: Option<tokio::time::Instant>,
    active: bool,
    stopped: Option<(tokio::time::Instant, Duration)>,
    frozen: bool,
    invalidated: bool,
    failures: usize,
    successes: usize,
    failure: Option<&'static str>,
}

struct Inner<S: RefinementService> {
    state: Mutex<State>,
    service: Arc<S>,
    progress: watch::Sender<PolishProgress>,
    released: Notify,
}

pub struct Polisher<S: RefinementService>(Arc<Inner<S>>);

fn valid_text(text: &str) -> bool {
    !text
        .chars()
        .any(|value| value.is_control() && !value.is_whitespace())
}

pub fn validate_edits(
    raw: &str,
    response: &DictationRefinement,
    base: usize,
) -> Option<Vec<Patch>> {
    if !valid_text(raw)
        || !valid_text(&response.text)
        || response.text.encode_utf16().count() > 8000
        || response.edits.len() > 4000
    {
        return None;
    }
    let mut patches = Vec::new();
    for edit in &response.edits {
        if edit.original.is_empty()
            || !valid_text(&edit.original)
            || !valid_text(&edit.replacement)
            || edit.replacement.encode_utf16().count() > 8000
        {
            return None;
        }
        let start = raw.find(&edit.original)?;
        // Search every valid next character boundary, not a byte inside a UTF-8 scalar.
        let next = raw[start..].chars().next().map(char::len_utf8)? + start;
        if raw[next..].contains(&edit.original) {
            return None;
        }
        let end = start + edit.original.len();
        if edit.replacement.is_empty()
            && start > 0
            && end < raw.len()
            && edit.original.chars().any(char::is_whitespace)
            && raw[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
            && raw[end..].chars().next().is_some_and(char::is_alphanumeric)
        {
            return None;
        }
        patches.push(Patch {
            start: base + start,
            end: base + end,
            replacement: edit.replacement.clone(),
        });
    }
    patches.sort_by_key(|patch| patch.start);
    let mut position = base;
    let mut assembled = String::new();
    for patch in &patches {
        if patch.start < position {
            return None;
        }
        assembled.push_str(&raw[position - base..patch.start - base]);
        assembled.push_str(&patch.replacement);
        position = patch.end;
        if assembled.encode_utf16().count() > 8000 {
            return None;
        }
    }
    assembled.push_str(&raw[position - base..]);
    (assembled == response.text).then_some(patches)
}

fn tokens(raw: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut begin = None;
    for (offset, value) in raw[start..end].char_indices() {
        if value.is_whitespace() {
            if let Some(begin) = begin.take() {
                words.push((begin, start + offset));
            }
        } else {
            begin.get_or_insert(start + offset);
        }
    }
    if let Some(begin) = begin {
        words.push((begin, end));
    }
    words
}

impl State {
    fn fail(&mut self, message: &'static str) {
        self.failures += 1;
        self.failure = Some(message);
        eprintln!("Dictation polishing fallback: {message}");
    }
    fn update_raw(&mut self, raw: &str) {
        if self.frozen {
            return;
        }
        if !raw.starts_with(&self.raw) {
            self.patches.clear();
            self.coverage.clear();
            self.arrivals.clear();
            if !self.invalidated {
                self.fail("Raw prefix changed; refinement disabled for this session.");
            }
            self.invalidated = true;
        }
        if raw.len() > self.raw.len() {
            if !raw[self.raw.len()..].trim().is_empty() {
                self.pending_since
                    .get_or_insert(tokio::time::Instant::now());
            }
            if !self.invalidated {
                self.arrivals
                    .push_back((raw.len(), tokio::time::Instant::now()));
            }
        }
        self.raw = raw.into();
    }
    fn covered(&self, word: (usize, usize)) -> bool {
        self.coverage
            .iter()
            .any(|(start, end)| *start <= word.0 && *end >= word.1)
    }
    fn progress(&self) -> PolishProgress {
        let mut text = String::new();
        let mut position = 0;
        for patch in &self.patches {
            text.push_str(&self.raw[position..patch.start]);
            text.push_str(&patch.replacement);
            position = patch.end;
        }
        text.push_str(&self.raw[position..]);
        PolishProgress {
            text,
            raw_text: self.raw.clone(),
            pending: self.active && !self.frozen && !self.invalidated,
            fallback_blocks: self.failures,
            successful_blocks: self.successes,
            last_failure: self.failure,
            unprocessed_words: tokens(&self.raw, 0, self.raw.len())
                .iter()
                .filter(|word| !self.covered(**word))
                .count(),
        }
    }
    fn dispatch(&mut self, force: bool) -> Option<Call> {
        if self.frozen
            || self.invalidated
            || self.active
            || (self.stopped.is_some() && !force)
            || self.raw.len() <= self.submitted
            || self.raw[self.submitted..].trim().is_empty()
        {
            return None;
        }
        while self
            .arrivals
            .front()
            .is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(30))
        {
            if let Some((end, _)) = self.arrivals.pop_front() {
                self.too_old = end;
            }
        }
        let trigger = match self.successes {
            0 => 20,
            1 => 40,
            _ => 75,
        };
        let delay = match self.successes {
            0 => 4,
            1 => 6,
            _ => 12,
        };
        let words = tokens(&self.raw, self.frontier, self.raw.len());
        if words.is_empty() {
            return None;
        }
        if !force
            && tokens(&self.raw, self.submitted, self.raw.len()).len() < trigger
            && !self
                .pending_since
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(delay))
            && self.raw[self.frontier..].encode_utf16().count() < 4000
        {
            return None;
        }
        let mut first = words.len().saturating_sub(75);
        let mut start = if first == 0 {
            self.frontier
        } else {
            words[first].0
        };
        let mut end = words.last()?.1;
        while first < words.len() && self.raw[start..end].encode_utf16().count() > 4000 {
            first += 1;
            start = words.get(first).map_or(end, |word| word.0);
        }
        start = start.max(self.too_old.min(end));
        while start < end && !self.raw.is_char_boundary(start) {
            start += 1;
        }
        if start > 0
            && start < end
            && !self.raw[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            start = self.raw[start..end]
                .char_indices()
                .find(|(_, value)| value.is_whitespace())
                .map_or(end, |(offset, _)| start + offset);
        }
        loop {
            let next = self
                .patches
                .iter()
                .filter(|patch| patch.start < start && start < patch.end)
                .map(|patch| patch.end)
                .max()
                .unwrap_or(start);
            if next == start {
                break;
            }
            start = next;
        }
        if start > self.frontier {
            if tokens(&self.raw, self.frontier, start)
                .iter()
                .any(|word| !self.covered(*word))
            {
                self.fail("Older or oversized unsubmitted speech retained raw.");
            }
            self.frontier = start;
        }
        if start == end {
            self.submitted = end;
            self.pending_since = None;
            return None;
        }
        for value in self.raw[end..].chars() {
            if !value.is_whitespace()
                || self.raw[start..end].encode_utf16().count() + value.len_utf16() > 4000
            {
                break;
            }
            end += value.len_utf8();
        }
        let earlier = tokens(&self.raw, 0, start);
        let lower = earlier
            .get(earlier.len().saturating_sub(150))
            .map_or(0, |word| word.0);
        let mut lower = lower;
        while lower < start && self.raw[lower..start].encode_utf16().count() > 8000 {
            lower += self.raw[lower..].chars().next()?.len_utf8();
        }
        let call = Call {
            start,
            end,
            raw: self.raw[start..end].into(),
            context: self.raw[lower..start].into(),
            overlap: match self.successes {
                0 => 5,
                1 => 10,
                _ => 15,
            },
            final_call: force,
        };
        self.active = true;
        self.submitted = end;
        self.pending_since = if end < self.raw.len() {
            Some(tokio::time::Instant::now())
        } else {
            None
        };
        Some(call)
    }
}

impl<S: RefinementService> Polisher<S> {
    pub fn new(service: Arc<S>) -> Self {
        let state = State {
            raw: String::new(),
            patches: Vec::new(),
            coverage: Vec::new(),
            arrivals: VecDeque::new(),
            frontier: 0,
            submitted: 0,
            too_old: 0,
            pending_since: None,
            active: false,
            stopped: None,
            frozen: false,
            invalidated: false,
            failures: 0,
            successes: 0,
            failure: None,
        };
        let (progress, _) = watch::channel(state.progress());
        Self(Arc::new(Inner {
            state: Mutex::new(state),
            service,
            progress,
            released: Notify::new(),
        }))
    }
    pub fn update(&self, raw: &str) {
        match self.0.state.lock() {
            Ok(mut state) => {
                state.update_raw(raw);
                let call = state.dispatch(false);
                self.0.progress.send_replace(state.progress());
                if let Some(call) = call {
                    execute(self.0.clone(), call);
                }
            }
            Err(_) => eprintln!("Polishing state lock is poisoned; raw text retained."),
        }
    }
    pub fn stop(&self, budget: Duration) {
        if let Ok(mut state) = self.0.state.lock() {
            state
                .stopped
                .get_or_insert((tokio::time::Instant::now(), budget));
        }
    }
    pub async fn complete(&self, raw: &str, budget: Duration) -> PolishProgress {
        self.stop(budget);
        let until = tokio::time::Instant::now() + budget;
        loop {
            let pending = match self.0.state.lock() {
                Ok(mut state) => {
                    state.update_raw(raw);
                    if state.frozen {
                        return state.progress();
                    }
                    let call = if tokio::time::Instant::now() < until {
                        state.dispatch(true)
                    } else {
                        None
                    };
                    if let Some(call) = call {
                        execute(self.0.clone(), call);
                    }
                    state.active
                }
                Err(_) => break,
            };
            if !pending || tokio::time::Instant::now() >= until {
                break;
            }
            tokio::select! { _ = tokio::time::sleep_until(until) => break, _ = self.0.released.notified() => {} }
        }
        self.freeze()
    }
    pub fn freeze(&self) -> PolishProgress {
        match self.0.state.lock() {
            Ok(mut state) => {
                state.frozen = true;
                let mut progress = state.progress();
                progress.pending = false;
                self.0.progress.send_replace(progress.clone());
                progress
            }
            Err(_) => {
                let mut progress = self.0.progress.borrow().clone();
                progress.pending = false;
                progress
            }
        }
    }
}
impl<S: RefinementService> Drop for Polisher<S> {
    fn drop(&mut self) {
        self.freeze();
    }
}

fn execute<S: RefinementService>(inner: Arc<Inner<S>>, call: Call) {
    tokio::spawn(async move {
        let skipped = match inner.state.lock() {
            Ok(mut state)
                if state.frozen
                    || state.invalidated
                    || (state.stopped.is_some() && !call.final_call) =>
            {
                state.active = false;
                inner.released.notify_one();
                true
            }
            Ok(_) => false,
            Err(_) => true,
        };
        if skipped {
            return;
        }
        let service = inner.service.clone();
        let request = call.clone();
        let mut underlying =
            tokio::spawn(async move { service.refine(&request.raw, &request.context).await });
        let mut finished = false;
        let outcome = match tokio::time::timeout(Duration::from_secs(25), &mut underlying).await {
            Ok(Ok(Ok(response))) => {
                finished = true;
                validate_edits(&call.raw, &response, call.start)
            }
            Ok(_) => {
                finished = true;
                None
            }
            Err(_) => None,
        };
        if let Ok(mut state) = inner.state.lock() {
            let on_time = state
                .stopped
                .is_none_or(|(at, budget)| at.elapsed() <= budget);
            if !state.frozen && !state.invalidated && on_time {
                if let Some(edits) = outcome {
                    state.patches.retain(|old| {
                        !(old.start >= call.start
                            && old.end <= call.end
                            && edits
                                .iter()
                                .any(|new| new.start < old.end && old.start < new.end))
                    });
                    state.patches.extend(edits);
                    state.patches.sort_by_key(|patch| patch.start);
                    if let Some(last) = state
                        .coverage
                        .last_mut()
                        .filter(|last| last.1 >= call.start)
                    {
                        last.1 = last.1.max(call.end);
                    } else {
                        state.coverage.push((call.start, call.end));
                    }
                    state.successes += 1;
                } else {
                    state.fail("Refinement failed or literal edit provenance was invalid; submitted speech retained raw.");
                }
                let words = tokens(&state.raw, call.start, call.end);
                let keep = call.overlap.min(words.len());
                let mut frontier = if keep == 0 {
                    call.end
                } else {
                    words[words.len() - keep].0
                };
                while let Some(start) = state
                    .patches
                    .iter()
                    .filter(|patch| patch.start < frontier && frontier < patch.end)
                    .map(|patch| patch.start)
                    .min()
                {
                    frontier = start;
                }
                state.frontier = frontier;
                inner.progress.send_replace(state.progress());
            }
        }
        // Keep the single physical request slot until an uncooperative call actually exits.
        if !finished {
            let _ = underlying.await;
        }
        if let Ok(mut state) = inner.state.lock() {
            state.active = false;
            let next = state.dispatch(false);
            inner.progress.send_replace(state.progress());
            inner.released.notify_one();
            if let Some(next) = next {
                execute(inner.clone(), next);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::DictationEdit;
    #[test]
    fn edit_provenance_is_literal_unique_nonoverlapping_and_cannot_join_words() {
        let valid = DictationRefinement {
            text: "hello Rust".into(),
            edits: vec![DictationEdit {
                original: "rust".into(),
                replacement: "Rust".into(),
            }],
        };
        assert_eq!(
            validate_edits("hello rust", &valid, 5).unwrap()[0].start,
            11
        );
        assert!(validate_edits("rust rust", &valid, 0).is_none());
        assert!(
            validate_edits(
                "hello rust",
                &DictationRefinement {
                    text: "hallucinated".into(),
                    edits: Vec::new()
                },
                0
            )
            .is_none()
        );
        assert!(
            validate_edits(
                "one um two",
                &DictationRefinement {
                    text: "onetwo".into(),
                    edits: vec![DictationEdit {
                        original: " um ".into(),
                        replacement: "".into()
                    }]
                },
                0
            )
            .is_none()
        );
        assert!(
            validate_edits(
                "\u{1f600} rust",
                &DictationRefinement {
                    text: "\u{1f600} Rust".into(),
                    edits: vec![DictationEdit {
                        original: "rust".into(),
                        replacement: "Rust".into()
                    }]
                },
                0
            )
            .is_some()
        );
    }

    struct Service {
        calls: std::sync::atomic::AtomicUsize,
        started: Notify,
        release: Notify,
    }
    impl RefinementService for Service {
        async fn refine(&self, text: &str, _: &str) -> Result<DictationRefinement, ApiError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            self.started.notify_one();
            self.release.notified().await;
            Ok(DictationRefinement {
                text: text.replace("rust", "Rust"),
                edits: vec![DictationEdit {
                    original: "rust".into(),
                    replacement: "Rust".into(),
                }],
            })
        }
    }

    #[tokio::test]
    async fn rolling_call_is_single_slot_and_late_output_cannot_mutate_frozen_delivery() {
        let service = Arc::new(Service {
            calls: std::sync::atomic::AtomicUsize::new(0),
            started: Notify::new(),
            release: Notify::new(),
        });
        let polisher = Polisher::new(service.clone());
        let raw = format!(
            "{} rust",
            (0..20)
                .map(|index| format!("word{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        polisher.update(&raw);
        service.started.notified().await;
        for _ in 0..10 {
            polisher.update(&format!("{raw} ending"));
        }
        assert_eq!(service.calls.load(std::sync::atomic::Ordering::Acquire), 1);
        let delivered = polisher
            .complete(&format!("{raw} ending"), Duration::ZERO)
            .await;
        assert_eq!(delivered.text, format!("{raw} ending"));
        assert_eq!(delivered.fallback_blocks, 0);
        service.release.notify_one();
        let until = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if !polisher.0.state.lock().unwrap().active {
                break;
            }
            assert!(tokio::time::Instant::now() < until);
            tokio::task::yield_now().await;
        }
        assert_eq!(polisher.freeze().text, delivered.text);
        assert_eq!(polisher.freeze().successful_blocks, 0);
    }

    #[tokio::test]
    async fn a_valid_response_is_applied_with_raw_tail_and_original_preserved() {
        let service = Arc::new(Service {
            calls: std::sync::atomic::AtomicUsize::new(0),
            started: Notify::new(),
            release: Notify::new(),
        });
        let polisher = Polisher::new(service.clone());
        let raw = format!(
            "{} rust",
            (0..20)
                .map(|index| format!("word{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        polisher.update(&raw);
        service.started.notified().await;
        service.release.notify_one();
        polisher.0.released.notified().await;
        polisher.update(&format!("{raw} original ending"));
        let result = polisher
            .complete(&format!("{raw} original ending"), Duration::ZERO)
            .await;
        assert!(result.text.ends_with("Rust original ending"));
        assert!(result.raw_text.ends_with("rust original ending"));
        assert_eq!(result.successful_blocks, 1);
        assert_eq!(result.fallback_blocks, 0);
        assert_eq!(result.unprocessed_words, 2);
    }
}
