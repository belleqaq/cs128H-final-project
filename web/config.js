// Brownshock — live config editor.
//
// Fetches the current GameConfig from the server on load, mirrors it into
// the form, and POSTs the full config on every slider/input change.  The
// server applies it on the next tick so the game reacts in real time.
//
// Each field has BOTH a range slider (clamped by HTML min/max) and a number
// input (unclamped — accepts any value).  Editing either syncs the other.

(() => {
  // Defaults match GameConfig::default() in Rust.
  const DEFAULTS = {
    tick_ms: 150,
    player: { max_speed: 1.0, acceleration: 0.3, friction: 0.85 },
    npc: { hold_min: 5, hold_max: 30, symbol: "N" },
  };

  const statusEl = document.getElementById("status");

  // -----------------------------------------------------------------------
  // Bidirectional slider ↔ number sync
  // -----------------------------------------------------------------------

  /** Set both the slider and its paired number input to `v`. */
  function setVal(sliderId, v) {
    const slider = document.getElementById(sliderId);
    const numInput = document.querySelector(`.num-input[data-for="${sliderId}"]`);
    if (slider) slider.value = v;
    if (numInput) numInput.value = v;
  }

  /** Read the authoritative value from the number input (unclamped). */
  function num(sliderId) {
    const numInput = document.querySelector(`.num-input[data-for="${sliderId}"]`);
    return parseFloat(numInput ? numInput.value : document.getElementById(sliderId).value);
  }
  function int(sliderId) {
    const numInput = document.querySelector(`.num-input[data-for="${sliderId}"]`);
    return parseInt(numInput ? numInput.value : document.getElementById(sliderId).value, 10);
  }

  // -----------------------------------------------------------------------
  // Form ↔ Config mapping
  // -----------------------------------------------------------------------

  function populateForm(cfg) {
    setVal("tick_ms", cfg.tick_ms);
    setVal("player_max_speed", cfg.player.max_speed);
    setVal("player_acceleration", cfg.player.acceleration);
    setVal("player_friction", cfg.player.friction);
    setVal("npc_hold_min", cfg.npc.hold_min);
    setVal("npc_hold_max", cfg.npc.hold_max);
    document.getElementById("npc_symbol").value = cfg.npc.symbol;
  }

  function readForm() {
    return {
      tick_ms: int("tick_ms"),
      player: {
        max_speed: num("player_max_speed"),
        acceleration: num("player_acceleration"),
        friction: num("player_friction"),
      },
      npc: {
        hold_min: int("npc_hold_min"),
        hold_max: Math.max(int("npc_hold_max"), int("npc_hold_min")),
        symbol: document.getElementById("npc_symbol").value || "N",
      },
    };
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

  const app = document.getElementById("config-app");

  // Slider change → sync number input, then POST.
  app.addEventListener("input", (e) => {
    const target = e.target;

    if (target.type === "range") {
      // Slider moved → update paired number input.
      const numInput = document.querySelector(`.num-input[data-for="${target.id}"]`);
      if (numInput) numInput.value = target.value;
    } else if (target.classList.contains("num-input")) {
      // Number typed → update paired slider (slider clamps naturally).
      const sliderId = target.dataset.for;
      const slider = document.getElementById(sliderId);
      if (slider) slider.value = target.value;
    }

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
