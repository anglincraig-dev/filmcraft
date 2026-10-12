//! Headless UI test of the Text panel's Transcript tab: the Transcribe button, the word view,
//! clicking words (In/Out from the selection) and extracting the selected text.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `transcript-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{Transcript, Word};
use filmcraft_speech::{Options, ProgressFn, SpeechError, Transcriber};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// Records the options `transcript.generate` passed to the recogniser.
struct Seen(Arc<Mutex<Vec<Options>>>);

impl Transcriber for Seen {
    fn id(&self) -> String {
        "seen".into()
    }
    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        if !progress(1.0, "Done") {
            return Err(SpeechError::Cancelled);
        }
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(opts.clone());
        let mut t = Transcript { language: opts.language.clone().unwrap_or_else(|| "und".into()), source: "seen".into(), ..Transcript::default() };
        if !audio.is_empty() {
            t.words.push(Word::new("Oi", Tick::ZERO, Tick::from_seconds_f64(0.2)));
        }
        Ok(t)
    }
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        Self::open(session)
    }

    fn open(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn app(&mut self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("transcript-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn transcript_tab_selects_and_extracts_words() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    d.frames(3);
    assert_eq!(d.ids("text.transcript.generate"), vec!["text.transcript.generate".to_string()], "empty state offers Transcribe");

    // bring a transcript for the first A1 clip's media (as an agent without a speech model would)
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
    let words: Vec<Value> = ["Hello", "um", "there", "friend."]
        .iter()
        .enumerate()
        .map(|(i, w)| json!({"text": w, "start": tk(0.5 + i as f64 * 0.5), "end": tk(0.9 + i as f64 * 0.5), "speaker": 0}))
        .collect();
    d.ok("engine.execute", json!({"command": "transcript.set", "params": {"item": a.item.0, "transcript": {"language": "en", "words": words}}}));
    d.frames(3);
    let ids = d.ids("text.transcript.word.");
    assert_eq!(ids.len(), 4, "{ids:?}");
    d.snapshot("words");

    d.ok("ui.click", json!({"id": "text.transcript.word.1"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.extract"}));
    d.frames(3);
    let r = d.ok("engine.execute", json!({"command": "transcript.inspect", "params": {}}));
    let left: Vec<&str> = r["words"].as_array().unwrap().iter().filter_map(|w| w["text"].as_str()).collect();
    assert_eq!(left, ["Hello", "there", "friend."]);
    assert_eq!(d.ids("text.transcript.word.").len(), 3);
}

/// Filler words and pauses are marked in the transcript, and Remove Pauses closes a long gap.
#[test]
fn transcript_marks_fillers_and_pauses_and_can_remove_the_pause() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    d.frames(3);
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
    let words = vec![
        json!({"text": "Hello", "start": tk(0.2), "end": tk(0.6), "speaker": 0}),
        json!({"text": "um", "start": tk(0.7), "end": tk(1.0), "speaker": 0}),
        json!({"text": "there", "start": tk(3.0), "end": tk(3.4), "speaker": 0}),
    ];
    d.ok("engine.execute", json!({"command": "transcript.set", "params": {"item": a.item.0, "transcript": {"language": "en", "words": words}}}));
    d.frames(3);
    assert!(d.ids("text.transcript.removePauses").contains(&"text.transcript.removePauses".to_string()));
    assert!(d.ids("text.transcript.removeFillers").contains(&"text.transcript.removeFillers".to_string()));
    let pauses = d.ids("text.transcript.pause.");
    assert!(!pauses.is_empty(), "a 2s gap is marked as a pause: {pauses:?}");
    d.ok("ui.click", json!({"id": "text.transcript.removePauses"}));
    d.frames(3);
    let r = d.ok("engine.execute", json!({"command": "transcript.inspect", "params": {}}));
    let starts: Vec<i64> = r["words"].as_array().unwrap().iter().filter_map(|w| w["sequenceStart"].as_i64().or_else(|| w["start"].as_i64())).collect();
    let gap = starts.windows(2).map(|w| w[1].saturating_sub(w[0])).max().unwrap_or(0);
    assert!(starts.len() >= 2 && gap < filmcraft_time::TICKS_PER_SECOND, "the long pause was closed, gap is {gap} ticks in {r}");
}

/// Transcribe opens a dialog. The chosen language and model are what recognition runs with.
#[test]
fn transcribe_dialog_sends_language_and_model() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    session.transcriber = Some(Arc::new(Seen(seen.clone())));
    let mut d = Driver::open(session);
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.generate"}));
    d.frames(3);

    let params = d.app().ui.extras.dialog.as_ref().expect("Transcribe opens a dialog").params.clone();
    assert_eq!(params["language"], "auto");
    assert_eq!(params["model"], "whisper-base");
    assert_eq!(params["diarize"], true);
    let ids = d.ids("transcribe.");
    for id in ["transcribe.language", "transcribe.model", "transcribe.diarize", "transcribe.cancel", "transcribe.ok"] {
        assert!(ids.iter().any(|got| got == id), "missing {id}: {ids:?}");
    }

    d.ok("ui.click", json!({"id": "transcribe.cancel"}));
    d.frames(2);
    assert!(d.app().ui.extras.dialog.is_none(), "Cancel closes the dialog");
    assert!(seen.lock().unwrap_or_else(|e| e.into_inner()).is_empty(), "Cancel does not transcribe");

    d.ok("ui.click", json!({"id": "text.transcript.generate"}));
    d.frames(2);
    d.ok("ui.set", json!({"menuDialog": {"language": "pt", "model": "whisper-small", "diarize": false}}));
    d.frames(1);
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().params["language"], "pt");
    d.ok("ui.click", json!({"id": "transcribe.ok"}));
    d.frames(3);

    let dialog = d.app().ui.extras.dialog.clone();
    assert!(dialog.is_none(), "Transcribe closes the dialog: {dialog:?}");
    let opts = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(!opts.is_empty(), "recognition ran");
    assert!(opts.iter().all(|o| o.language.as_deref() == Some("pt") && !o.diarize), "Portuguese, speakers off: {opts:?}");
    let sent = d.app().session.journal.iter().find(|(id, p)| id == "sequence.transcribe" && p["language"] == "pt").map(|(_, p)| p.clone());
    let p = sent.expect("sequence.transcribe was journaled");
    assert_eq!(p["model"], "whisper-small");
    assert_eq!(p["diarize"], false);
}
