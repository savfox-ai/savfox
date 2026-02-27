use std::io;
use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub enum LabelColor {
    Assistant,
    Tool,
    ToolMeta,
    Session,
}

#[derive(Debug, Default)]
struct PromptState {
    session_id: Option<String>,
    visible: bool,
}

#[derive(Clone, Debug)]
pub struct Output {
    lock: Arc<Mutex<()>>,
    prompt: Arc<Mutex<PromptState>>,
    color: bool,
}

impl Output {
    pub fn new() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let color = !no_color && io::stdout().is_terminal() && io::stderr().is_terminal();
        Self {
            lock: Arc::new(Mutex::new(())),
            prompt: Arc::new(Mutex::new(PromptState::default())),
            color,
        }
    }

    pub fn server_line(&self, line: &str) -> io::Result<()> {
        let _guard = self.lock.lock().expect("output lock poisoned");
        self.clear_prompt_line_locked()?;
        let mut stdout = io::stdout();
        writeln!(stdout, "{line}")?;
        stdout.flush()?;
        self.redraw_prompt_locked()
    }

    pub fn client_line(&self, line: &str) -> io::Result<()> {
        let _guard = self.lock.lock().expect("output lock poisoned");
        self.clear_prompt_line_locked()?;
        let mut stderr = io::stderr();
        writeln!(stderr, "{line}")?;
        stderr.flush()
    }

    pub fn prompt(&self, session_id: &str) -> io::Result<()> {
        let _guard = self.lock.lock().expect("output lock poisoned");
        self.set_prompt_locked(session_id);
        self.write_prompt_locked()
    }

    pub fn set_prompt(&self, session_id: &str) {
        let _guard = self.lock.lock().expect("output lock poisoned");
        self.set_prompt_locked(session_id);
    }

    pub fn format_label(&self, label: &str, color: LabelColor) -> String {
        if !self.color {
            return label.to_string();
        }

        let code = match color {
            LabelColor::Assistant => "32",
            LabelColor::Tool => "36",
            LabelColor::ToolMeta => "33",
            LabelColor::Session => "34",
        };
        format!("\x1b[{code}m{label}\x1b[0m")
    }

    fn clear_prompt_line_locked(&self) -> io::Result<()> {
        let mut prompt = self.prompt.lock().expect("prompt lock poisoned");
        if prompt.visible {
            let mut stderr = io::stderr();
            writeln!(stderr)?;
            stderr.flush()?;
            prompt.visible = false;
        }
        Ok(())
    }

    fn redraw_prompt_locked(&self) -> io::Result<()> {
        if self
            .prompt
            .lock()
            .expect("prompt lock poisoned")
            .session_id
            .is_some()
        {
            self.write_prompt_locked()?;
        }
        Ok(())
    }

    fn set_prompt_locked(&self, session_id: &str) {
        let mut prompt = self.prompt.lock().expect("prompt lock poisoned");
        prompt.session_id = Some(session_id.to_string());
    }

    fn write_prompt_locked(&self) -> io::Result<()> {
        let mut prompt = self.prompt.lock().expect("prompt lock poisoned");
        let Some(session_id) = prompt.session_id.as_ref() else {
            return Ok(());
        };
        let mut stderr = io::stderr();
        write!(stderr, "({session_id})> ")?;
        stderr.flush()?;
        prompt.visible = true;
        Ok(())
    }
}
