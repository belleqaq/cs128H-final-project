// Brownshock — live config editor.
//
// Fetches the current GameConfig from the server on load, mirrors it into
// the form, and POSTs the full config on every slider/input change.  The
// server applies it on the next tick so the game reacts in real time.

(() => {
  // Defaults match GameConfig::default() in Rust. Used by "Reset to defaults".
  const DEFAULTS = {
    tick_ms: 150,
    player: { speed: 1.0, acceleration: 0.3, keep_momentum: 1.0, friction: 0.6 },
    npc: { wait_min: 1, wait_max: 10, move_distance: 1, symbol: "N" },
  };

  const statusEl = document.getElementById("status");

  // -----------------------------------------------------------------------
  // Form ↔ Config mapping
  // -----------------------------------------------------------------------

  function populateForm(cfg) {
    setVal("tick_ms", cfg.tick_ms);
    setVal("player_speed", cfg.player.speed);
    setVal("player_acceleration", cfg.player.acceleration);
    setVal("player_keep_momentum", cfg.player.keep_momentum);
    setVal("player_friction", cfg.player.friction);
    setVal("npc_wait_min", cfg.npc.wait_min);
    setVal("npc_wait_max", cfg.npc.wait_max);
    setVal("npc_move_distance", cfg.npc.move_distance);
    document.getElementById("npc_symbol").value = cfg.npc.symbol;
    syncAllOutputs();
  }

  function readForm() {
    return {
      tick_ms: int("tick_ms"),
      player: {
        speed: num("player_speed"),
        acceleration: num("player_acceleration"),
        keep_momentum: num("player_keep_momentum"),
        friction: num("player_friction"),
      },
      npc: {
        wait_min: int("npc_wait_min"),
        wait_max: Math.max(int("npc_wait_max"), int("npc_wait_min")),
        move_distance: int("npc_move_distance"),
        symbol: document.getElementById("npc_symbol").value || "N",
      },
    };
  }

  function setVal(id, v) {
    document.getElementById(id).value = v;
  }
  function num(id) {
    return parseFloat(document.getElementById(id).value);
  }
  function int(id) {
    return parseInt(document.getElementById(id).value, 10);
  }

  // -----------------------------------------------------------------------
  // <output> display sync
  // -----------------------------------------------------------------------

  function syncAllOutputs() {
    for (const input of document.querySelectorAll("input[type=range]")) {
      const out = document.querySelector(`output[for="${input.id}"]`);
      if (out) out.textContent = input.value;
    }
  }

  // -----------------------------------------------------------------------
  // Network
  // -----------------------------------------------------------------------

  let debounceTimer = null;

  async function applyConfig() {
    const cfg = readForm();
    try {
      const resp = await fetch("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(cfg),
      });
      flash(resp.ok ? "Applied" : "Error " + resp.status, resp.ok);
    } catch (e) {
      flash("Network error", false);
    }
  }

  function scheduleApply() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(applyConfig, 80);
  }

  async function fetchAndPopulate() {
    try {
      const resp = await fetch("/api/config");
      if (!resp.ok) throw new Error(resp.status);
      populateForm(await resp.json());
      flash("Loaded", true);
    } catch (e) {
      flash("Failed to load config", false);
    }
  }

  // -----------------------------------------------------------------------
  // Status flash
  // -----------------------------------------------------------------------

  let flashTimer = null;
  function flash(msg, ok) {
    statusEl.textContent = msg;
    statusEl.className = "status " + (ok ? "status-ok" : "status-err");
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => {
      statusEl.textContent = "";
      statusEl.className = "status";
    }, 1500);
  }

  // -----------------------------------------------------------------------
  // Events
  // -----------------------------------------------------------------------

  // Every slider/input change → debounced POST.
  document.getElementById("config-app").addEventListener("input", (e) => {
    syncAllOutputs();
    scheduleApply();
  });

  document.getElementById("btn-defaults").addEventListener("click", () => {
    populateForm(DEFAULTS);
    applyConfig();
  });

  document.getElementById("btn-save").addEventListener("click", async () => {
    try {
      const resp = await fetch("/api/config/save", { method: "POST" });
      flash(resp.ok ? "Saved to config.toml" : "Save failed", resp.ok);
    } catch (e) {
      flash("Network error", false);
    }
  });

  // Init.
  fetchAndPopulate();
})();
