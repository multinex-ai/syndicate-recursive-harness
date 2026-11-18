(() => {
  const params = new URLSearchParams(window.location.search);
  const urlToken = params.get("token");
  if (urlToken) {
    sessionStorage.setItem("forge_token", urlToken);
    window.history.replaceState({}, document.title, "/");
  }

  const tokenInput = document.getElementById("tokenInput");
  tokenInput.value = sessionStorage.getItem("forge_token") || "";
  tokenInput.addEventListener("change", () => {
    sessionStorage.setItem("forge_token", tokenInput.value.trim());
    refreshAll();
  });

  const themes = ["system", "dark", "light"];
  let themeIdx = Math.max(0, themes.indexOf(localStorage.getItem("forge_theme") || "system"));
  const themeBtn = document.getElementById("themeBtn");
  function applyTheme() {
    const mode = themes[themeIdx];
    document.documentElement.setAttribute("data-theme", mode);
    themeBtn.textContent = `Theme: ${mode[0].toUpperCase()}${mode.slice(1)}`;
    localStorage.setItem("forge_theme", mode);
  }
  themeBtn.addEventListener("click", () => {
    themeIdx = (themeIdx + 1) % themes.length;
    applyTheme();
  });
  applyTheme();

  async function api(path, options = {}) {
    const tok = (tokenInput.value || sessionStorage.getItem("forge_token") || "").trim();
    const headers = { ...(options.headers || {}) };
    if (tok) headers.Authorization = `Bearer ${tok}`;
    if (options.body && !headers["Content-Type"]) headers["Content-Type"] = "application/json";
    const res = await fetch(path, { ...options, headers });
    const text = await res.text();
    let data;
    try { data = JSON.parse(text); } catch { data = { raw: text }; }
    if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
    return data;
  }

  async function refreshAll() {
    const healthBadge = document.getElementById("healthBadge");
    try {
      const h = await fetch("/api/health").then((r) => r.json());
      healthBadge.textContent = h.ok ? "loopback online" : "degraded";
      healthBadge.className = `badge ${h.ok ? "ok" : "warn"}`;
    } catch {
      healthBadge.textContent = "offline";
      healthBadge.className = "badge warn";
    }

    try {
      const m = await api("/api/machine");
      const lockBadge = document.getElementById("lockBadge");
      lockBadge.textContent = m.model_lock_held ? "LOCK HELD" : "LOCK FREE";
      lockBadge.className = `badge ${m.model_lock_held ? "warn" : "ok"}`;
      const avail = m.sample?.available_bytes ? `${(m.sample.available_bytes / 1073741824).toFixed(2)} GiB` : "—";
      document.getElementById("memAvail").textContent = avail;
      document.getElementById("memPressure").textContent = m.sample?.pressure_level ?? "—";
      document.getElementById("admissionVerdict").textContent = m.admission_blocked_reason || "READY";
      document.getElementById("machineRaw").textContent = JSON.stringify(m, null, 2);
    } catch (err) {
      document.getElementById("machineRaw").textContent = `Auth required: ${err.message}`;
    }

    try {
      const [roster, summary] = await Promise.all([api("/api/roster"), api("/api/summary")]);
      const roles = roster.roster || [];
      document.getElementById("rosterCount").textContent = `${roles.length} roles`;
      document.getElementById("rosterOutput").textContent = JSON.stringify({ roster: roles, summary }, null, 2);
    } catch (err) {
      document.getElementById("rosterOutput").textContent = `Could not load roster: ${err.message}`;
    }
  }

  let activeMission = null;
  let pollTimer = null;
  const launchBtn = document.getElementById("launchBtn");
  const stopBtn = document.getElementById("stopBtn");
  const missionBadge = document.getElementById("missionBadge");
  const missionOutput = document.getElementById("missionOutput");

  async function pollMission() {
    if (!activeMission) return;
    try {
      const q = new URLSearchParams({ job_id: activeMission.job_id, run_dir: activeMission.run_dir || "" });
      const st = await api(`/api/mission/status?${q}`);
      missionOutput.textContent = JSON.stringify(st, null, 2);
      missionBadge.textContent = st.running ? "running" : "finished";
      missionBadge.className = `badge ${st.running ? "warn" : "ok"}`;
      stopBtn.disabled = !st.running;
      if (!st.running && pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    } catch (err) {
      missionOutput.textContent = `Status error: ${err.message}`;
    }
  }

  launchBtn.addEventListener("click", async () => {
    const role = document.getElementById("roleSelect").value;
    const objective = document.getElementById("objectiveInput").value.trim();
    if (!objective) {
      missionOutput.textContent = "Enter a mission objective before dispatching.";
      return;
    }
    try {
      const res = await api("/api/mission", {
        method: "POST",
        body: JSON.stringify({ role, objective, acceptance_criteria: {} }),
      });
      activeMission = res.mission;
      stopBtn.disabled = false;
      missionBadge.textContent = "running";
      missionBadge.className = "badge warn";
      missionOutput.textContent = JSON.stringify(res, null, 2);
      if (pollTimer) clearInterval(pollTimer);
      pollTimer = setInterval(pollMission, 2000);
    } catch (err) {
      missionOutput.textContent = `Launch refused: ${err.message}`;
    }
  });

  stopBtn.addEventListener("click", async () => {
    if (!activeMission) return;
    try {
      const res = await api("/api/mission/stop", {
        method: "POST",
        body: JSON.stringify({ job_id: activeMission.job_id }),
      });
      missionOutput.textContent = JSON.stringify(res, null, 2);
      stopBtn.disabled = true;
      missionBadge.textContent = "stopped";
    } catch (err) {
      missionOutput.textContent = `Stop error: ${err.message}`;
    }
  });

  document.getElementById("refreshBtn").addEventListener("click", refreshAll);
  refreshAll();
})();
