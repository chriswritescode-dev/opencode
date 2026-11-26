use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use opencode_shell::scan::{Command, ScanResult, scan_power_shell};
use serde::Deserialize;

const ORACLE: &str = r#"
$ErrorActionPreference = 'Stop'
$probe = 'Invoke-ProbeB'
$script:seen = [System.Collections.Generic.List[string]]::new()
function Invoke-ProbeA { [void]$script:seen.Add('Invoke-ProbeA'); 1 }
function Invoke-ProbeB { [void]$script:seen.Add('Invoke-ProbeB'); 1 }
function Invoke-ProbeC { [void]$script:seen.Add('Invoke-ProbeC'); 1 }
function Invoke-ProbeBlock {
  [void]$script:seen.Add('Invoke-ProbeBlock')
  foreach ($argument in $args) {
    if ($argument -is [scriptblock]) { & $argument }
  }
}
$results = foreach ($source in (ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd()))) {
  $tokens = $null
  $errors = $null
  $ast = [System.Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
  $nodes = @($ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] }, $true))
  $commands = @($nodes | ForEach-Object {
    @{
      name = $_.GetCommandName()
      text = $_.Extent.Text
      start = $_.Extent.StartOffset
      end = $_.Extent.EndOffset
    }
  })
  $script:seen.Clear()
  $runtimeError = $null
  $unsafe = @($nodes | Where-Object {
    $name = $_.GetCommandName()
    ($name -and $name -notin @('Invoke-ProbeA', 'Invoke-ProbeB', 'Invoke-ProbeC', 'Invoke-ProbeBlock', 'ForEach-Object')) -or
    (!$name -and $_.CommandElements[0] -isnot [System.Management.Automation.Language.ScriptBlockExpressionAst])
  })
  $files = @($ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.FileRedirectionAst] -and $node.Location.Extent.Text -ne '$null'
  }, $true))
  $background = @($ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.PipelineAst] -and $node.Background
  }, $true))
  if ($errors.Count -eq 0 -and $unsafe.Count -eq 0 -and $files.Count -eq 0 -and $background.Count -eq 0) {
    try { & ([scriptblock]::Create($source)) | Out-Null }
    catch { $runtimeError = $_.Exception.Message }
  }
  @{
    source = $source
    commands = $commands
    errors = @($errors | ForEach-Object { $_.ErrorId })
    executed = @($script:seen.ToArray())
    runtimeError = $runtimeError
  }
}
ConvertTo-Json -InputObject @($results) -Depth 10 -Compress
"#;

#[derive(Deserialize)]
struct Recorded {
    source: String,
    input: String,
}

#[derive(Deserialize)]
struct RuntimeResult {
    source: String,
    commands: Vec<RuntimeCommand>,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    executed: Vec<String>,
}

#[derive(Deserialize)]
struct RuntimeCommand {
    name: Option<String>,
    text: String,
}

fn recorded(line: &str) -> Vec<String> {
    let corpus = fs::read_to_string(format!("{}/tests/corpus/scan-powershell.json", env!("CARGO_MANIFEST_DIR")))
        .expect("scan-powershell corpus");
    let calls: Vec<Recorded> = serde_json::from_str(&corpus).expect("scan-powershell corpus json");
    calls.into_iter().filter(|call| call.source.ends_with(line)).map(|call| call.input).collect()
}

fn find_pwsh() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SHELL_SCAN_PWSH") {
        return Some(PathBuf::from(path));
    }
    which("pwsh")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| candidate.is_file())
}

fn run(command: &mut Process, timeout: Duration) -> bool {
    let mut child = command.spawn().expect("start pwsh");
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("wait for pwsh") {
            return status.success();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn matches(candidate: &Command, name: &str, text: &str) -> bool {
    candidate.words.first().is_some_and(|word| word.to_lowercase() == name.to_lowercase())
        && candidate.resource == text.trim()
}

#[test]
fn successful_powershell_scans_cover_real_parser_boundaries_and_executed_probe_calls() {
    let Some(pwsh) = find_pwsh() else {
        eprintln!("pwsh not found; skipping the PowerShell runtime oracle");
        return;
    };
    let fixtures = recorded("powershell-runtime.test.ts:569");
    let supported = recorded("powershell-runtime.test.ts:35");
    assert_eq!(fixtures.len(), 4466, "PowerShell runtime fixture list");
    assert_eq!(supported.len(), 24, "PowerShell supported fixture list");
    let directory = std::env::temp_dir().join(format!("opencode-shell-pwsh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("create pwsh directory");
    let input = directory.join("fixtures.json");
    let output = directory.join("results.json");
    let error = directory.join("stderr.txt");
    fs::write(&input, serde_json::to_vec(&fixtures).expect("fixture json")).expect("write fixtures");
    let mut command = Process::new(&pwsh);
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", ORACLE])
        .stdin(Stdio::from(fs::File::open(&input).expect("open fixtures")))
        .stdout(Stdio::from(fs::File::create(&output).expect("create results")))
        .stderr(Stdio::from(fs::File::create(&error).expect("create stderr")));
    let success = run(&mut command, Duration::from_secs(45));
    let stderr = fs::read_to_string(&error).unwrap_or_default();
    assert!(success, "pwsh failed: {stderr}");
    assert!(stderr.is_empty(), "pwsh wrote to stderr: {stderr}");
    let text = fs::read_to_string(&output).expect("read results");
    let json = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let results: Vec<RuntimeResult> = serde_json::from_str(json)
        .unwrap_or_else(|error| panic!("invalid pwsh output: {error}: {}", json.chars().take(500).collect::<String>()));
    assert_eq!(results.len(), fixtures.len());
    let supported: HashSet<&str> = supported.iter().map(String::as_str).collect();
    let mut failures = Vec::new();
    let mut scanned = 0;
    let mut executed = 0;
    for result in &results {
        let scan = scan_power_shell(&result.source);
        if supported.contains(result.source.as_str()) {
            assert!(result.errors.is_empty(), "{}", result.source);
            assert!(matches!(scan, ScanResult::Scanned { .. }), "{}", result.source);
        }
        let commands = match &scan {
            ScanResult::Scanned { commands } => commands,
            ScanResult::Opaque { .. } => continue,
        };
        if !result.errors.is_empty() {
            continue;
        }
        scanned += 1;
        executed += result.executed.len();
        let missing: Vec<&str> = result
            .commands
            .iter()
            .filter(|command| {
                command
                    .name
                    .as_deref()
                    .is_some_and(|name| !commands.iter().any(|candidate| matches(candidate, name, &command.text)))
            })
            .map(|command| command.text.as_str())
            .collect();
        let unobserved: Vec<&str> = result
            .executed
            .iter()
            .filter(|name| {
                !commands.iter().any(|candidate| {
                    candidate.words.first().is_some_and(|word| word.to_lowercase() == name.to_lowercase())
                })
            })
            .map(String::as_str)
            .collect();
        if !missing.is_empty() || !unobserved.is_empty() {
            failures.push(format!(
                "{{source: {:?}, missing: {:?}, unobserved: {:?}, scanned: {:?}}}",
                result.source,
                missing,
                unobserved,
                commands.iter().map(|command| command.words.first().map(String::as_str)).collect::<Vec<_>>()
            ));
        }
    }
    assert!(scanned > 100, "only {scanned} fixtures were scanned");
    assert!(executed > 100, "only {executed} probe calls were executed");
    assert!(
        failures.is_empty(),
        "{} PowerShell runtime failures:\n{}",
        failures.len(),
        failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
    let _ = fs::remove_dir_all(&directory);
}
