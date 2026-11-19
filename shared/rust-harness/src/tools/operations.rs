impl Tools {
    // ---------- files ----------

    /// Python Path.resolve(): resolve symlinks of existing parts, `..` lexically for the rest.
    pub fn path(&self, rel: &str) -> R<PathBuf> {
        let p = resolve(&self.workspace.join(rel));
        if !p.starts_with(&self.workspace) {
            return err("ValueError", "Path is outside the selected workspace");
        }
        Ok(p)
    }

    fn read_file(&self, rel: &str) -> R<Json> {
        let p = self.path(rel)?;
        let bytes = fs::read(&p).or_else(|e| io_err(e, &p))?;
        let text = String::from_utf8(bytes).or_else(|e| err("UnicodeDecodeError", e))?;
        Ok(json!(text.chars().take(16000).collect::<String>()))
    }

    fn write_file(&self, rel: &str, content: &str) -> R<Json> {
        let p = self.path(rel)?;
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).or_else(|e| io_err(e, d))?;
        }
        fs::write(&p, content).or_else(|e| io_err(e, &p))?;
        Ok(
            json!({"path": p.to_string_lossy(), "bytes": fs::metadata(&p).map(|m| m.len()).unwrap_or(0)}),
        )
    }

    fn run_verify_script(&self, script_rel: &str, rel_path: &str) -> R<Json> {
        let target = if rel_path.trim().is_empty() || rel_path.trim() == "." {
            self.workspace.clone()
        } else {
            self.path(rel_path)?
        };
        let script = crate::jobs::root().join(script_rel);
        let out = std::process::Command::new("node")
            .arg(&script)
            .arg(&target)
            .output()
            .map_err(|e| ToolError(format!("OSError: {e}")))?;
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str::<Json>(text.trim()).or_else(|_| {
            err(
                "RuntimeError",
                format!(
                    "{script_rel} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            )
        })
    }

    fn list_files(&self) -> Json {
        let mut out = vec![];
        let mut stack = vec![self.workspace.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect();
            entries.sort();
            for p in entries.into_iter().rev() {
                let link = fs::symlink_metadata(&p)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(true);
                if p.is_dir() {
                    if !link {
                        stack.push(p);
                    }
                } else if p.is_file() {
                    out.push(
                        p.strip_prefix(&self.workspace)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        out.sort();
        out.truncate(200);
        json!(out)
    }

    // ---------- shell ----------

    fn run_command(&self, argv: &[String]) -> R<Json> {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().or_else(|e| io_err(e, Path::new(&argv[0])))?;
        let (code, timed_out, out, errs) = self.wait_cancellable(child, Duration::from_secs(30))?;
        let mut r =
            json!({"exit_code": code, "stdout": tail(&out, 12000), "stderr": tail(&errs, 4000)});
        if timed_out {
            r["timeout"] = json!(true);
        }
        Ok(r)
    }

    /// Wait for a child in its own process group; kill the group on timeout or cancel.
    /// Returns (exit code, timed out, stdout, stderr).
    pub fn wait_cancellable(
        &self,
        mut child: Child,
        limit: Duration,
    ) -> R<(Option<i32>, bool, String, String)> {
        let reader = |r: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut b = vec![];
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut b);
                }
                String::from_utf8_lossy(&b).into_owned()
            })
        };
        let out = reader(
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        );
        let errs = reader(
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        );
        let deadline = Instant::now() + limit;
        let pgid = child.id() as i32;
        let mut timed_out = false;
        let status = loop {
            if let Ok(Some(s)) = child.try_wait() {
                break s;
            }
            let cancel = (self.cancelled)();
            if cancel || Instant::now() >= deadline {
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
                let s = child
                    .wait()
                    .map_err(|e| ToolError(format!("OSError: {e}")))?;
                if cancel {
                    return Err(ToolError(CANCELLED.into()));
                }
                timed_out = true;
                break s;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        // Grandchildren that kept the pipes open would block the readers forever.
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
        use std::os::unix::process::ExitStatusExt;
        let code = status.code().or_else(|| status.signal().map(|s| -s));
        Ok((
            code,
            timed_out,
            out.join().unwrap_or_default(),
            errs.join().unwrap_or_default(),
        ))
    }

    // ---------- local messages ----------

    fn send_message(
        &self,
        recipient: &str,
        message: &str,
        needs_followup: bool,
        preset: &str,
        reference: Option<&str>,
    ) -> R<Json> {
        if recipient == self.role {
            return err(
                "ValueError",
                "Execute this role’s assignment directly; self-delegation is not supported.",
            );
        }
        if let Some(m) = crate::syndicate::membership(&self.root, &self.role) {
            if m.can_message(recipient).is_none() {
                return err(
                    "PermissionError",
                    &format!(
                        "No relationship allows {} to message {recipient}.",
                        self.role
                    ),
                );
            }
        }
        if recipient == "image-studio" || recipient == "video-studio" {
            crate::jobs::media_arguments(recipient, preset, reference)
                .map_err(|e| ToolError(format!("ValueError: {e}")))?;
        } else if preset != "draft" || reference.is_some() {
            return err("ValueError", "Media options require a media recipient");
        }
        let dir = crate::agent::inbox_dir(&self.root, recipient);
        fs::create_dir_all(&dir).or_else(|e| io_err(e, &dir))?;
        let id = crate::jobs::new_job_id().map_err(|e| ToolError(format!("OSError: {e}")))?;
        let envelope = json!({"id": id, "sender": self.role, "recipient": recipient, "message": message, "needs_followup": needs_followup,
            "status": "queued", "created_unix": now(), "allowed_hosts": self.allowed_hosts, "origin_task": self.task,
            "media_preset": preset, "reference_image": reference});
        let target = dir.join(format!("{id}.json"));
        fs::write(
            &target,
            serde_json::to_string_pretty(&envelope).unwrap_or_default(),
        )
        .or_else(|e| io_err(e, &target))?;
        Ok(
            json!({"message_id": id, "status": "queued", "path": target.to_string_lossy(), "note": "Recipient execution is separate; delivery is a local file only."}),
        )
    }

    fn receive_messages(&self) -> R<Json> {
        let dir = crate::agent::inbox_dir(&self.root, &self.role);
        fs::create_dir_all(&dir).or_else(|e| io_err(e, &dir))?;
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|t| (t, p))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        files
            .into_iter()
            .take(20)
            .map(|(_, p)| {
                let t = fs::read_to_string(&p).or_else(|e| io_err(e, &p))?;
                serde_json::from_str(&t).or_else(|e| err("JSONDecodeError", e))
            })
            .collect::<R<Vec<Json>>>()
            .map(Json::Array)
    }

    /// Append a status note to <run_dir>/status-reports.jsonl. Durable, structured, and surfaced
    /// by `telemetry`/`report` as an uncertainty when risk is medium/high or uncertainty is non-empty.
    fn report_status(&self, progress: &str, uncertainty: &str, risk: &str) -> R<Json> {
        if !["none", "low", "medium", "high"].contains(&risk) {
            return err("ValueError", "risk must be none, low, medium or high");
        }
        let entry = json!({"at_unix": now(), "progress": progress, "uncertainty": uncertainty, "risk": risk});
        fs::create_dir_all(&self.run_dir).or_else(|e| io_err(e, &self.run_dir))?;
        let path = self.run_dir.join("status-reports.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .or_else(|e| io_err(e, &path))?;
        use std::io::Write;
        writeln!(f, "{entry}").map_err(|e| ToolError(e.to_string()))?;
        Ok(json!({"recorded": true, "path": path}))
    }

    // ---------- workers and skills ----------

    fn list_workers(&self) -> Json {
        let workers: Vec<Json> = CAPABILITIES.iter().map(|(role, model, capability)| {
            let cfg: Json = fs::read_to_string(self.root.join("agents").join(role).join("config.json")).ok()
                .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
            let missing = prerequisites(&self.root, role);
            let tests: &[&str] = match *role {
                "coordinator" => &["communication", "lifecycle", "memory-agent", "media-routing"],
                "cyber-lab" => &["cyber"], "browser" => &["browser"], "desktop" => &["desktop", "vision"], _ => &[],
            };
            let read = |p: PathBuf| fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Json>(&t).ok()).unwrap_or(json!({}));
            let mut qualification: Vec<Json> = tests.iter().map(|t| {
                let d = read(self.root.join("evidence/setup-20261102").join(format!("{t}-acceptance.json")));
                json!({"test": t, "passed": d.get("passed") == Some(&json!(true))})
            }).collect();
            if role.ends_with("-studio") {
                let d = read(self.root.join("agents").join(role).join("outputs/local-smoke.evidence.json"));
                qualification.push(json!({"test": "generation_and_visual_review", "passed": d.get("visual_inspection") == Some(&json!("passed"))}));
            }
            let defaults: Map<String, Json> = ["width", "height", "frames", "fps", "steps", "vision"].iter()
                .filter_map(|k| cfg.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
            json!({"role": role, "model": model, "capability": capability, "installation_ready": missing.is_empty(), "missing": missing,
                   "last_recorded_qualification": qualification, "defaults": defaults})
        }).collect();
        let gib = |b: u64| ((b as f64 / (1u64 << 30) as f64) * 100.0).round() / 100.0;
        let resources = crate::runtime::Sample::take(None).map(|s| json!({"physical_memory_gib": gib(s.physical_bytes),
            "available_memory_gib": gib(s.available_bytes), "swap_used_gib": gib(s.swap_used_bytes)})).unwrap_or(json!({}));
        json!({"workers": workers, "resources": resources,
               "resource_policy": "One package-managed heavy model process at a time. Release the parent before dispatch. No idle model residency. Other applications and model servers are not controlled.",
               "readiness_is_not_task_acceptance": true})
    }

    fn skill_catalog(&self) -> R<Vec<Json>> {
        let p = self.root.join("shared/config/skills.json");
        let t = fs::read_to_string(&p).or_else(|e| io_err(e, &p))?;
        let v: Json = serde_json::from_str(&t).or_else(|e| err("JSONDecodeError", e))?;
        Ok(v["skills"].as_array().cloned().unwrap_or_default())
    }

    fn list_skills(&self) -> R<Json> {
        Ok(Json::Array(self.skill_catalog()?.iter().map(|s| json!({"name": s["name"], "description": s["description"], "capability_note": s["capability_note"]})).collect()))
    }

    fn read_skill(&self, name: &str, reference: &str) -> R<Json> {
        let entry = self
            .skill_catalog()?
            .into_iter()
            .find(|s| s["name"] == name)
            .ok_or_else(|| ToolError("ValueError: Skill is not in the installed catalog".into()))?;
        let main = self.root.join(entry["path"].as_str().unwrap_or_default());
        let dir = main.parent().map(resolve).unwrap_or_default();
        let target = resolve(&dir.join(reference));
        if !target.starts_with(&dir) || target.extension().map(|e| e != "md").unwrap_or(true) {
            return err(
                "ValueError",
                "Skill reference must be a Markdown file inside this skill",
            );
        }
        let main_bytes = fs::read(dir.join("SKILL.md")).or_else(|e| io_err(e, &main))?;
        if hex::encode(Sha256::digest(&main_bytes)) != entry["sha256"].as_str().unwrap_or_default()
        {
            return err(
                "ValueError",
                "Installed skill changed; refresh its reviewed manifest",
            );
        }
        let text = fs::read_to_string(&target).or_else(|e| io_err(e, &target))?;
        if text.chars().count() > 24000 {
            return err("ValueError", "Skill reference exceeds 24,000 characters");
        }
        let mut refs = vec![];
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for p in fs::read_dir(&d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
            {
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "md") {
                    refs.push(
                        p.strip_prefix(&dir)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        refs.sort();
        refs.truncate(80);
        Ok(
            json!({"name": name, "reference": reference, "text": text, "guidance_only": true, "available_references": refs}),
        )
    }

    // ---------- MemQ ----------
    // Status and cached schemas are config reads, done natively. Live calls need the
    // MCP OAuth (PKCE + streamable HTTP over TLS) client in memq_client.py, which stays
    // Python; it runs as a killable child so a call can still be cancelled.

    fn memq_config(&self) -> R<Json> {
        let p = self.root.join("shared/config/memq.json");
        let mut cfg: Json =
            serde_json::from_str(&fs::read_to_string(&p).or_else(|e| io_err(e, &p))?)
                .or_else(|e| err("JSONDecodeError", e))?;
        if cfg["binding"] == json!({"source": "own-client-private-binding"}) {
            let b = memq_private_dir().join("binding.json");
            cfg["binding"] = match fs::symlink_metadata(&b) {
                Err(_) => Json::Null,
                Ok(m) => {
                    use std::os::unix::fs::PermissionsExt;
                    if m.file_type().is_symlink() || m.permissions().mode() & 0o077 != 0 {
                        return err("PermissionError", "Binding file must be owner-only");
                    }
                    serde_json::from_str(&fs::read_to_string(&b).or_else(|e| io_err(e, &b))?)
                        .or_else(|e| err("JSONDecodeError", e))?
                }
            };
        }
        Ok(cfg)
    }

    /// Enforce explicit project exclusions configured by the local operator.
    pub fn memq_check_scope(&self, cfg: &Json) -> R<()> {
        let project = resolve(&self.workspace);
        let mut paths = vec![project.clone()];
        if let Ok(o) = Command::new("git")
            .args([
                "-C",
                &project.to_string_lossy(),
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ])
            .stderr(Stdio::null())
            .output()
        {
            if o.status.success() {
                paths.push(resolve(Path::new(
                    String::from_utf8_lossy(&o.stdout).trim(),
                )));
            }
        }
        let blocked: Vec<PathBuf> = cfg["excluded_project_roots"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .map(|p| resolve(Path::new(p)))
            .collect();
        if paths
            .iter()
            .any(|p| blocked.iter().any(|b| p.starts_with(b)))
        {
            return err("PermissionError", "This project is excluded from the configured memory provider. Select the correct verified provider before memory access.");
        }
        Ok(())
    }

    fn memq_status(&self) -> R<Json> {
        let cfg = self.memq_config()?;
        self.memq_check_scope(&cfg)?;
        Ok(
            json!({"configured": cfg["enabled"], "expected_account": cfg["expected_account"], "namespace_organization": cfg["expected_namespace_organization"],
            "own_oauth_material_present": memq_private_dir().join("oauth-tokens.json").is_file(), "binding_configured": truthy(&cfg["binding"]),
            "live_identity_verified": false,
            "identity_check_scope": "Configuration-only status; each memq_call performs fresh identity and namespace verification before private access and after the response.",
            "allowed_tools": cfg["allowed_tools"], "other_client_credentials_used": false}),
        )
    }

    fn memq_tools(&self) -> R<Json> {
        let cfg = self.memq_config()?;
        self.memq_check_scope(&cfg)?;
        if !truthy(&cfg["binding"]) {
            return Ok(json!({"tools": [], "reason": "Verified binding not yet configured"}));
        }
        let cached = self.root.join("shared/runtime/memq/discovered-tools.json");
        let Ok(text) = fs::read_to_string(&cached) else {
            return Ok(json!({"tools": [], "reason": "Run client discovery first"}));
        };
        let allowed = cfg["allowed_tools"].as_array().cloned().unwrap_or_default();
        let v: Json = serde_json::from_str(&text).or_else(|e| err("JSONDecodeError", e))?;
        let tools: Vec<Json> = v["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| allowed.contains(&t["name"]))
            .map(|t| {
                let mut schema = t["inputSchema"].clone();
                if let Some(p) = schema.get_mut("properties").and_then(Json::as_object_mut) {
                    p.remove("namespace");
                }
                json!({"name": t["name"], "description": t["description"], "inputSchema": schema})
            })
            .collect();
        Ok(
            json!({"tools": tools, "cached_metadata_only": true, "live_identity_verified": false,
                  "identity_check_scope": "Schema discovery only; memq_call validates live identity and the current schema before executing."}),
        )
    }

    fn memq_call(&self, name: &str, arguments: &Json) -> R<Json> {
        let cfg = self.memq_config()?;
        self.memq_check_scope(&cfg)?;
        let script = self.root.join("shared/harness/memq_client.py");
        let mut cmd = Command::new(&self.memq_python);
        cmd.arg(&script)
            .args(["call", "--name", name, "--project"])
            .arg(&self.workspace)
            .args(["--task", &self.task, "--arguments", &arguments.to_string()])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().or_else(|e| io_err(e, &self.memq_python))?;
        let (code, timed_out, out, _) = self.wait_cancellable(child, Duration::from_secs(120))?;
        if timed_out {
            return err(
                "TimeoutError",
                "MemQ call exceeded 120 seconds; reconcile this operation before retrying",
            );
        }
        let reply: Json = out
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str(l).ok())
            .ok_or_else(|| {
                ToolError(format!(
                    "RuntimeError: MemQ client exited ({code:?}) without a reply"
                ))
            })?;
        if reply["ok"] == json!(true) {
            Ok(reply["result"].clone())
        } else {
            Err(ToolError(
                reply["error"]
                    .as_str()
                    .unwrap_or("RuntimeError: MemQ call failed")
                    .into(),
            ))
        }
    }

    // ---------- browser ----------

    pub fn url_allowed(&self, url: &str) -> bool {
        browser::url_allowed(url, &self.allowed_hosts)
    }

    fn browser_tool(&mut self, name: &str, a: &Map<String, Json>) -> R<Json> {
        let s = |k: &str| {
            a.get(k)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        if name == "browser_open" {
            let url = s("url");
            if !self.url_allowed(&url) {
                return err("ValueError", "Host is not in the run allowlist");
            }
            if self.browser.is_none() {
                self.browser = Some(browser::Browser::launch(
                    &self.run_dir,
                    self.allowed_hosts.clone(),
                    &*self.cancelled,
                )?);
            }
        }
        let shot = self
            .run_dir
            .join(format!("browser-{}.png", self.events.len()));
        let cancelled = &*self.cancelled;
        let b = self
            .browser
            .as_mut()
            .ok_or_else(|| ToolError("ValueError: Open a page first".into()))?;
        match name {
            "browser_open" => {
                b.goto(&s("url"), cancelled)?;
                b.observe(cancelled)
            }
            "browser_observe" => b.observe(cancelled),
            "browser_click" => {
                b.click(&s("selector"), cancelled)?;
                b.observe(cancelled)
            }
            "browser_fill" => {
                b.fill(&s("selector"), &s("text"), cancelled)?;
                b.observe(cancelled)
            }
            "browser_screenshot" => {
                b.screenshot(&shot, cancelled)?;
                Ok(json!({"image_path": shot.to_string_lossy()}))
            }
            _ => err("ValueError", format!("Unknown tool: {name}")),
        }
    }

    pub fn close(&mut self) {
        if let Some(b) = self.browser.take() {
            b.close();
        }
    }
}
