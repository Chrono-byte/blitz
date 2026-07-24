use zed_extension_api::{self as zed, Result};

struct BlitzExtension;

impl zed::Extension for BlitzExtension {
  fn new() -> Self {
    Self
  }

  fn language_server_command(
    &mut self,
    _language_server_id: &zed::LanguageServerId,
    worktree: &zed::Worktree,
  ) -> Result<zed::Command> {
    let local_server = "language-server/src/lsp.ts";
    if worktree.read_text_file(local_server).is_ok()
      && let Some(command) = worktree.which("bun")
    {
      return Ok(zed::Command {
        command,
        args: vec![format!("{}/{}", worktree.root_path(), local_server)],
        env: worktree.shell_env(),
      });
    }

    if let Some(command) = worktree.which("blitz-language-server") {
      return Ok(zed::Command {
        command,
        args: Vec::new(),
        env: worktree.shell_env(),
      });
    }

    Err(
      "blitz-language-server is not installed; run `bun run build && bun link` in the Blitz language-server directory"
        .to_string(),
    )
  }
}

zed::register_extension!(BlitzExtension);
