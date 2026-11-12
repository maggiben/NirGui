import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import "./styles.css";

// ---------------------------------------------------------------- types (mirror Rust serde)

type RefMode = "builtin" | "local" | "none";

interface SlewSection {
  section_scan_type: number;
  width_px: number;
  wavelength_start_nm: number;
  wavelength_end_nm: number;
  num_patterns: number;
  exposure_time: number;
}
interface SlewScanConfig {
  scan_type: number;
  scan_config_index: number;
  serial_number: string;
  config_name: string;
  num_repeats: number;
  num_sections: number;
  sections: SlewSection[];
}
interface DeviceInfo {
  serial: string;
  model: string;
  tiva_version: string;
  dlpc_sw_version: string;
  dlpc_flash_version: string;
  spec_lib_version: string;
  cal_data_version: string;
  ref_cal_version: string;
  cfg_data_version: string;
  path: string;
}
interface StatusInfo {
  present: boolean;
  connected: boolean;
  device_status: number;
  error_status: number;
  error_codes: number[];
  battery_volts: number | null;
  busy: boolean;
}
interface ScanView {
  file_stem: string;
  meta: {
    scan_name: string;
    timestamp: string;
    serial_number: string;
    config_name: string;
    num_repeats: number;
    pga: number;
    system_temp_c: number;
    detector_temp_c: number;
    humidity_pct: number;
    lamp_pd: number;
    scan_data_index: number;
    length: number;
    sections: { scan_type: string; start_nm: number; end_nm: number; width_px: number; width_nm: number; num_patterns: number; exposure_us: number }[];
  };
  wavelength: number[];
  intensity: number[];
  reference: number[] | null;
  absorbance: (number | null)[] | null;
  reflectance: (number | null)[] | null;
  reference_mode: RefMode;
  reference_error: string | null;
  extra: Record<string, unknown>;
}
interface ConfigCheck {
  ok: boolean;
  errors: string[];
  max_patterns: (number | null)[];
  total_patterns: number;
}

// ---------------------------------------------------------------- helpers

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const esc = (s: unknown) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);

let toastTimer: number | undefined;
function toast(msg: string, isError = false) {
  const t = $("toast");
  t.textContent = msg;
  t.className = "toast" + (isError ? " error" : "");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => t.classList.add("hidden"), isError ? 7000 : 3500);
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw new Error(String(e));
  }
}

const EXP_TIMES = ["0.635 ms", "1.27 ms", "2.45 ms", "5.08 ms", "15.24 ms", "30.48 ms", "60.96 ms"];
const PX_NM = (1700 - 900) / (854 * 0.8);

// ---------------------------------------------------------------- state

const state = {
  connected: false,
  present: false,
  busy: false,
  info: null as DeviceInfo | null,
  configs: [] as SlewScanConfig[],
  active: 0,
  selectedCfg: -1,
  cfgDirty: false,
  view: null as ScanView | null,
  history: [] as ScanView[],
  plotType: "absorbance" as "absorbance" | "reflectance" | "intensity" | "reference",
};

// ---------------------------------------------------------------- navigation

document.querySelectorAll<HTMLButtonElement>(".nav").forEach((b) =>
  b.addEventListener("click", () => {
    document.querySelectorAll(".nav").forEach((x) => x.classList.remove("active"));
    b.classList.add("active");
    document.querySelectorAll(".view").forEach((v) => v.classList.add("hidden"));
    $(`view-${b.dataset.view}`).classList.remove("hidden");
    if (b.dataset.view === "sensors" && state.connected) refreshSensors();
    if (b.dataset.view === "scan") resizePlot();
  }),
);

// ---------------------------------------------------------------- connection

async function connect() {
  const btn = $<HTMLButtonElement>("btn-connect");
  btn.disabled = true;
  try {
    const info = await call<DeviceInfo>("connect");
    state.info = info;
    state.connected = true;
    toast(`Connected to NIRscan Nano ${info.serial}`);
    renderInfo();
    await loadConfigs();
    await updateEstimate();
    await refreshLocalRef();
  } catch (e) {
    toast(String((e as Error).message), true);
  } finally {
    btn.disabled = false;
    renderConn();
  }
}

async function disconnect() {
  await call("disconnect");
  state.connected = false;
  renderConn();
}

$("btn-connect").addEventListener("click", () => (state.connected ? disconnect() : connect()));

let autoConnectTried = false;
async function poll() {
  try {
    const s = await call<StatusInfo>("poll_status");
    const wasConnected = state.connected;
    state.present = s.present;
    state.connected = s.connected;
    state.busy = s.busy;
    if (wasConnected && !s.connected) toast("NIRscan Nano disconnected", true);
    if (s.present && !s.connected && !autoConnectTried) {
      autoConnectTried = true;
      await connect();
      return;
    }
    if (!s.present) autoConnectTried = false;
    if (s.connected && !s.busy) {
      $("chip-scan").classList.toggle("hidden", (s.device_status & 0x02) === 0);
      $("chip-sd").classList.toggle("hidden", (s.device_status & 0x04) === 0);
      $("chip-ble").classList.toggle("hidden", (s.device_status & 0x20) === 0);
      const batt = $("chip-batt");
      batt.classList.toggle("hidden", s.battery_volts == null);
      if (s.battery_volts != null) batt.textContent = `Battery ${s.battery_volts.toFixed(2)} V`;
      // TMP006 (bit 0x80) is obsolete and not fitted on current EVMs; TI says the error is benign.
      const errs = s.error_status & ~0x80;
      const err = $("chip-err");
      err.classList.toggle("hidden", errs === 0);
      err.textContent = `Errors 0x${errs.toString(16).toUpperCase()}`;
      err.title = `Device error bits: ${describeErrors(errs)}. Click to clear.`;
    }
    if (!s.connected) ["chip-scan", "chip-sd", "chip-ble", "chip-batt", "chip-err"].forEach((id) => $(id).classList.add("hidden"));
  } catch (e) {
    console.warn(e);
  }
  renderConn();
}

const ERROR_BITS = ["scan", "ADC", "SD card", "EEPROM", "BLE", "spectrum library", "hardware", "TMP006", "HDC1000", "battery empty", "memory", "UART"];
function describeErrors(bits: number) {
  return ERROR_BITS.filter((_, i) => bits & (1 << i)).join(", ") || "none";
}

$("chip-err").addEventListener("click", async () => {
  try {
    await call("reset_errors");
    toast("Device errors cleared");
  } catch (e) {
    toast((e as Error).message, true);
  }
});

function renderConn() {
  const dot = $("conn-dot");
  dot.className = "dot " + (state.connected ? "on" : state.present ? "present" : "off");
  $("conn-text").textContent = state.connected
    ? `${state.info?.model || "NIRscan Nano"} · SN ${state.info?.serial ?? ""}`
    : state.present
      ? "Device detected"
      : "Not connected";
  $("btn-connect").textContent = state.connected ? "Disconnect" : "Connect";
  $<HTMLButtonElement>("btn-connect").disabled = !state.connected && !state.present;
  $<HTMLButtonElement>("btn-scan").disabled = !state.connected || scanning;
}

// ---------------------------------------------------------------- device view

function renderInfo() {
  const i = state.info;
  if (!i) return;
  const rows: [string, string][] = [
    ["Model", i.model || "–"],
    ["Serial number", i.serial],
    ["Tiva firmware", i.tiva_version],
    ["DLPC150 firmware", i.dlpc_sw_version],
    ["DLPC flash image", i.dlpc_flash_version],
    ["Spectrum library (device)", i.spec_lib_version],
    ["Calibration data version", i.cal_data_version],
    ["Reference cal version", i.ref_cal_version],
    ["Config data version", i.cfg_data_version],
  ];
  $("dev-info").innerHTML = rows.map(([k, v]) => `<tr><td>${esc(k)}</td><td>${esc(v)}</td></tr>`).join("");
}

interface Sensors {
  ambient_c: number | null;
  detector_c: number | null;
  humidity_pct: number | null;
  hdc_temp_c: number | null;
  battery_v: number | null;
  tiva_temp_c: number | null;
  photodetector: [number, number, number] | null;
  device_time: string | null;
  pga: number | null;
  hibernate: boolean | null;
  sd_scans: number | null;
  errors: string[];
}

let sensorsBusy = false;
async function refreshSensors() {
  if (sensorsBusy || !state.connected || scanning) return;
  sensorsBusy = true;
  try {
    const s = await call<Sensors>("read_sensors");
    const tile = (k: string, v: number | null, unit: string, digits: number, sub = "") =>
      v == null
        ? sub.startsWith("TMP006")
          ? `<div class="tile"><div class="k">${esc(k)}</div><div class="v"><small>not fitted</small></div><div class="s">TMP006 is obsolete; not used for scans (TI)</div></div>`
          : `<div class="tile bad"><div class="k">${esc(k)}</div><div class="v">not available</div><div class="s">${esc(sub)}</div></div>`
        : `<div class="tile"><div class="k">${esc(k)}</div><div class="v">${v.toFixed(digits)}<small>${unit}</small></div><div class="s">${esc(sub)}</div></div>`;
    const pd = s.photodetector;
    $("sens-tiles").innerHTML = [
      tile("Battery Voltage", s.battery_v, "V", 2, s.battery_v === 0 ? "no battery / USB powered" : "BQ24250"),
      tile("Ambient Temp", s.ambient_c, "°C", 2, "TMP006"),
      tile("Detector Temp", s.detector_c, "°C", 2, "TMP006 (InGaAs detector)"),
      tile("Humidity", s.humidity_pct, "%", 2, "HDC1000"),
      tile("HDC Temp", s.hdc_temp_c, "°C", 2, "HDC1000 board temperature"),
      tile("Tiva Temp", s.tiva_temp_c, "°C", 2, "TM4C129 internal"),
      tile("Photodetector", pd ? pd[1] : null, "", 0, pd ? `lamp monitor · R ${pd[0]} / G ${pd[1]} / B ${pd[2]}` : "lamp monitor"),
    ].join("");
    $("sens-updated").textContent = `Updated ${new Date().toLocaleTimeString()}` + (s.device_time ? ` · device clock ${s.device_time}` : "");
    $("sens-errors").textContent = s.errors.filter((e) => !e.startsWith("TMP006")).join("\n");
    if (s.hibernate != null) $<HTMLInputElement>("dev-hibernate").checked = s.hibernate;
  } catch (e) {
    $("sens-errors").textContent = (e as Error).message;
  } finally {
    sensorsBusy = false;
  }
}
$("btn-read-sensors").addEventListener("click", refreshSensors);
setInterval(() => {
  if ($<HTMLInputElement>("sens-auto").checked && !$("view-sensors").classList.contains("hidden")) refreshSensors();
}, 2000);

function devAction(id: string, fn: () => Promise<string | void>) {
  $(id).addEventListener("click", async () => {
    try {
      const m = await fn();
      if (m) {
        $("dev-msg").textContent = m;
        toast(m);
      }
    } catch (e) {
      toast((e as Error).message, true);
    }
  });
}
devAction("btn-clock", async () => `Device clock set to ${await call<string>("sync_clock")}`);
devAction("btn-lamp-on", async () => {
  await call("set_lamp", { mode: "on" });
  return "Lamp on (stays on between scans)";
});
devAction("btn-lamp-auto", async () => {
  await call("set_lamp", { mode: "auto" });
  return "Lamp automatic";
});
devAction("btn-sd-read", async () => {
  const v = await call<ScanView>("read_sd_last_scan", { reference: refMode() });
  showScan(v);
  document.querySelector<HTMLButtonElement>('.nav[data-view="scan"]')!.click();
  return "Loaded last scan from SD card";
});
devAction("btn-sd-del", async () => {
  if (!confirm("Delete the last scan stored on the SD card? This cannot be undone.")) return;
  await call("delete_sd_last_scan");
  return "Last SD card scan deleted";
});
devAction("btn-reset", async () => {
  if (!confirm("Reset the NIRscan Nano? It will disconnect and reboot.")) return;
  await call("reset_device");
  state.connected = false;
  autoConnectTried = false;
  renderConn();
  return "Device reset";
});
$<HTMLInputElement>("dev-hibernate").addEventListener("change", async (e) => {
  try {
    await call("set_hibernate", { enabled: (e.target as HTMLInputElement).checked });
  } catch (err) {
    toast((err as Error).message, true);
  }
});

// ---------------------------------------------------------------- tests

document.querySelectorAll<HTMLButtonElement>("[data-test]").forEach((b) =>
  b.addEventListener("click", async () => {
    const log = $("test-log");
    const name = b.textContent;
    log.textContent += `${new Date().toLocaleTimeString()}  ${name}: running…\n`;
    b.disabled = true;
    try {
      const r = await call<string>("run_test", { name: b.dataset.test });
      log.textContent += `${new Date().toLocaleTimeString()}  ${name}: ${r}\n`;
    } catch (e) {
      log.textContent += `${new Date().toLocaleTimeString()}  ${name}: ERROR ${(e as Error).message}\n`;
    } finally {
      b.disabled = false;
      log.scrollTop = log.scrollHeight;
    }
  }),
);

// ---------------------------------------------------------------- configurations

async function loadConfigs() {
  try {
    const r = await call<{ configs: SlewScanConfig[]; active: number }>("list_configs");
    state.configs = r.configs;
    state.active = r.active;
    state.selectedCfg = r.configs.length ? r.active : -1;
    state.cfgDirty = false;
    renderConfigList();
    renderEditor();
    renderScanConfigSelect();
  } catch (e) {
    toast(`Reading scan configurations failed: ${(e as Error).message}`, true);
  }
}

function cfgSummary(c: SlewScanConfig) {
  const secs = c.sections.slice(0, c.num_sections);
  const lo = Math.min(...secs.map((s) => s.wavelength_start_nm));
  const hi = Math.max(...secs.map((s) => s.wavelength_end_nm));
  const types = [...new Set(secs.map((s) => (s.section_scan_type === 1 ? "Hadamard" : "Column")))].join("+");
  const pts = secs.reduce((a, s) => a + s.num_patterns, 0);
  return `${types} · ${lo}–${hi} nm · ${pts} pts · ×${c.num_repeats}`;
}

function renderConfigList() {
  $("cfg-list").innerHTML = state.configs
    .map(
      (c, i) =>
        `<tr data-i="${i}" class="${i === state.selectedCfg ? "sel" : ""}"><td><b>${esc(c.config_name)}</b>${
          i === state.active ? '<span class="badge">● active</span>' : ""
        }<div class="hint">${esc(cfgSummary(c))}</div></td></tr>`,
    )
    .join("");
  $("cfg-list")
    .querySelectorAll("tr")
    .forEach((tr) =>
      tr.addEventListener("click", () => {
        state.selectedCfg = Number(tr.dataset.i);
        renderConfigList();
        renderEditor();
      }),
    );
  $("cfg-dirty").textContent = state.cfgDirty ? "Unsaved changes. Press “Write list to device” to store them." : "";
}

function renderScanConfigSelect() {
  const sel = $<HTMLSelectElement>("scan-config");
  sel.innerHTML = state.configs.map((c, i) => `<option value="${i}">${esc(c.config_name)}</option>`).join("");
  sel.value = String(state.active);
}

function newSection(): SlewSection {
  return { section_scan_type: 0, width_px: 6, wavelength_start_nm: 900, wavelength_end_nm: 1700, num_patterns: 228, exposure_time: 0 };
}
function defaultConfig(): SlewScanConfig {
  return {
    scan_type: 2,
    scan_config_index: 0,
    serial_number: state.info?.serial ?? "",
    config_name: "New config",
    num_repeats: 6,
    num_sections: 1,
    sections: [newSection(), newSection(), newSection(), newSection(), newSection()],
  };
}
function normalize(c: SlewScanConfig): SlewScanConfig {
  const secs = [...c.sections];
  while (secs.length < 5) secs.push(newSection());
  return { ...c, sections: secs };
}

let validateTimer: number | undefined;
function renderEditor() {
  const ed = $("cfg-editor");
  const c = state.configs[state.selectedCfg];
  if (!c) {
    ed.className = "hint";
    ed.textContent = state.connected ? "Select or create a configuration." : "Connect the device to manage configurations.";
    return;
  }
  ed.className = "";
  const widthOpts = (sel: number) => {
    let o = "";
    for (let px = 2; px <= 52; px++) o += `<option value="${px}" ${px === sel ? "selected" : ""}>${(PX_NM * px).toFixed(2)} nm (${px} px)</option>`;
    return o;
  };
  const rows = c.sections
    .slice(0, c.num_sections)
    .map(
      (s, i) => `<tr data-s="${i}">
      <td><select data-f="section_scan_type"><option value="0" ${s.section_scan_type === 0 ? "selected" : ""}>Column</option><option value="1" ${s.section_scan_type === 1 ? "selected" : ""}>Hadamard</option></select></td>
      <td><input data-f="wavelength_start_nm" type="number" min="900" max="1700" value="${s.wavelength_start_nm}"></td>
      <td><input data-f="wavelength_end_nm" type="number" min="900" max="1700" value="${s.wavelength_end_nm}"></td>
      <td><select data-f="width_px">${widthOpts(s.width_px)}</select></td>
      <td><input data-f="num_patterns" type="number" min="2" max="624" value="${s.num_patterns}"><div class="hint" data-max="${i}"></div></td>
      <td><select data-f="exposure_time">${EXP_TIMES.map((t, k) => `<option value="${k}" ${k === s.exposure_time ? "selected" : ""}>${t}</option>`).join("")}</select></td>
    </tr>`,
    )
    .join("");
  ed.innerHTML = `
    <label>Name <input id="cfg-name" type="text" maxlength="39" value="${esc(c.config_name)}"></label>
    <div class="row" style="margin-top:0">
      <label>Scans to average <input id="cfg-repeats" type="number" min="1" max="65535" value="${c.num_repeats}"></label>
      <label>Sections <select id="cfg-nsec">${[1, 2, 3, 4, 5].map((n) => `<option ${n === c.num_sections ? "selected" : ""}>${n}</option>`).join("")}</select></label>
    </div>
    <table class="sections">
      <tr><th>Method</th><th>Start nm</th><th>End nm</th><th>Pattern width</th><th>Points</th><th>Exposure</th></tr>
      ${rows}
    </table>
    <div class="hint" id="cfg-total"></div>
    <div class="errors" id="cfg-errors"></div>`;
  const touch = () => {
    state.cfgDirty = true;
    $("cfg-dirty").textContent = "Unsaved changes. Press “Write list to device” to store them.";
    clearTimeout(validateTimer);
    validateTimer = window.setTimeout(validateSelected, 250);
  };
  $<HTMLInputElement>("cfg-name").addEventListener("input", (e) => {
    c.config_name = (e.target as HTMLInputElement).value;
    renderConfigListLight();
    touch();
  });
  $<HTMLInputElement>("cfg-repeats").addEventListener("input", (e) => {
    c.num_repeats = Number((e.target as HTMLInputElement).value) || 1;
    touch();
  });
  $<HTMLSelectElement>("cfg-nsec").addEventListener("change", (e) => {
    c.num_sections = Number((e.target as HTMLSelectElement).value);
    touch();
    renderEditor();
  });
  ed.querySelectorAll<HTMLElement>("tr[data-s]").forEach((tr) => {
    const s = c.sections[Number(tr.dataset.s)];
    tr.querySelectorAll<HTMLInputElement | HTMLSelectElement>("[data-f]").forEach((inp) =>
      inp.addEventListener("input", () => {
        (s as unknown as Record<string, number>)[inp.dataset.f!] = Number(inp.value);
        touch();
      }),
    );
  });
  validateSelected();
}

function renderConfigListLight() {
  const tr = $("cfg-list").querySelector(`tr[data-i="${state.selectedCfg}"] b`);
  if (tr) tr.textContent = state.configs[state.selectedCfg].config_name;
}

async function validateSelected() {
  const c = state.configs[state.selectedCfg];
  if (!c || !$("cfg-errors")) return;
  try {
    const r = await call<ConfigCheck>("validate_config", { config: c });
    $("cfg-errors").textContent = r.errors.join("\n");
    $("cfg-total").textContent = `Total points: ${r.total_patterns} / 624`;
    r.max_patterns.forEach((m, i) => {
      const el = document.querySelector(`[data-max="${i}"]`);
      if (el) el.textContent = m == null ? "" : `max ${m}`;
    });
  } catch (e) {
    $("cfg-errors").textContent = (e as Error).message;
  }
}

$("btn-cfg-reload").addEventListener("click", loadConfigs);
$("btn-cfg-new").addEventListener("click", () => {
  state.configs.push(defaultConfig());
  state.selectedCfg = state.configs.length - 1;
  state.cfgDirty = true;
  renderConfigList();
  renderEditor();
});
$("btn-cfg-dup").addEventListener("click", () => {
  const c = state.configs[state.selectedCfg];
  if (!c) return;
  const d = normalize(JSON.parse(JSON.stringify(c)));
  d.config_name = (c.config_name + " copy").slice(0, 39);
  state.configs.push(d);
  state.selectedCfg = state.configs.length - 1;
  state.cfgDirty = true;
  renderConfigList();
  renderEditor();
});
$("btn-cfg-del").addEventListener("click", () => {
  if (state.selectedCfg < 0) return;
  if (state.configs.length <= 1) return toast("The device needs at least one configuration", true);
  state.configs.splice(state.selectedCfg, 1);
  if (state.active >= state.configs.length) state.active = 0;
  else if (state.active > state.selectedCfg) state.active--;
  state.selectedCfg = Math.min(state.selectedCfg, state.configs.length - 1);
  state.cfgDirty = true;
  renderConfigList();
  renderEditor();
});
$("btn-cfg-active").addEventListener("click", async () => {
  if (state.selectedCfg < 0) return;
  state.active = state.selectedCfg;
  if (!state.cfgDirty) {
    try {
      await call("set_active_config", { index: state.active });
      toast("Active configuration changed");
      await updateEstimate();
    } catch (e) {
      toast((e as Error).message, true);
    }
  } else state.cfgDirty = true;
  renderConfigList();
  renderScanConfigSelect();
});
$("btn-cfg-save").addEventListener("click", async () => {
  if (!confirm("Replace all scan configurations stored on the device with this list?")) return;
  try {
    const configs = state.configs.map(normalize);
    await call("save_configs", { configs, active: state.active });
    toast("Configurations written to device");
    await loadConfigs();
    await updateEstimate();
  } catch (e) {
    toast((e as Error).message, true);
  }
});

// ---------------------------------------------------------------- scanning

let scanning = false;
const refMode = () => $<HTMLSelectElement>("scan-ref").value as RefMode;

async function updateEstimate() {
  if (!state.connected) return;
  try {
    const ms = await call<number>("estimate_scan_time", { repeats: Number($<HTMLInputElement>("scan-repeats").value) || 1 });
    $("scan-est").textContent = `Estimated time: ${(ms / 1000).toFixed(1)} s`;
  } catch {
    $("scan-est").textContent = "Estimated time: –";
  }
}
$("scan-repeats").addEventListener("change", updateEstimate);
$("scan-config").addEventListener("change", async () => {
  const idx = Number($<HTMLSelectElement>("scan-config").value);
  try {
    await call("set_active_config", { index: idx });
    state.active = idx;
    $<HTMLInputElement>("scan-repeats").value = String(state.configs[idx]?.num_repeats ?? 6);
    renderConfigList();
    await updateEstimate();
  } catch (e) {
    toast((e as Error).message, true);
  }
});

async function refreshLocalRef() {
  const info = await call<string | null>("local_reference_info").catch(() => null);
  $("local-ref-info").textContent = info ? `Local reference: ${info}` : "No local reference set";
}

listen<{ stage: string; elapsed_ms: number; estimated_ms: number }>("scan-progress", (e) => {
  const p = e.payload;
  const pct = p.stage === "processing" ? 100 : Math.min(98, (p.elapsed_ms / Math.max(1, p.estimated_ms)) * 100);
  $("scan-bar").style.width = `${pct}%`;
  $("scan-status").textContent =
    p.stage === "processing" ? "Downloading and processing…" : `Scanning… ${(p.elapsed_ms / 1000).toFixed(1)} s of ~${(p.estimated_ms / 1000).toFixed(1)} s`;
});

$("btn-scan").addEventListener("click", async () => {
  if (scanning) return;
  scanning = true;
  renderConn();
  $("scan-progress").classList.remove("hidden");
  $("scan-bar").style.width = "0";
  $("scan-status").textContent = "Starting scan…";
  const pga = $<HTMLSelectElement>("scan-pga").value;
  try {
    const v = await call<ScanView>("scan", {
      opts: {
        repeats: Number($<HTMLInputElement>("scan-repeats").value) || 1,
        store_in_sd: $<HTMLInputElement>("scan-sd").checked,
        name_tag: $<HTMLInputElement>("scan-tag").value,
        pga: pga === "auto" ? null : Number(pga),
        lamp: $<HTMLSelectElement>("scan-lamp").value,
        reference: refMode(),
        config: null,
      },
    });
    const ms = Number(v.extra?.scan_time_ms ?? 0);
    $("scan-status").textContent = `Scan done in ${(ms / 1000).toFixed(1)} s · ${v.meta.length} points`;
    showScan(v);
  } catch (e) {
    $("scan-status").textContent = "Scan failed";
    toast((e as Error).message, true);
  } finally {
    scanning = false;
    $("scan-progress").classList.add("hidden");
    renderConn();
  }
});

$("scan-ref").addEventListener("change", async () => {
  if (!state.view) return;
  try {
    const v = await call<ScanView>("recompute", { reference: refMode() });
    state.history.pop();
    showScan(v);
  } catch (e) {
    toast((e as Error).message, true);
  }
});

$("btn-save").addEventListener("click", async () => {
  try {
    const p = await call<string | null>("save_scan");
    if (p) toast(`Saved ${p} (and raw .dat)`);
  } catch (e) {
    toast((e as Error).message, true);
  }
});
$("btn-open").addEventListener("click", async () => {
  try {
    const v = await call<ScanView | null>("open_scan", { reference: refMode() });
    if (v) showScan(v);
  } catch (e) {
    toast((e as Error).message, true);
  }
});
$("btn-setref").addEventListener("click", async () => {
  try {
    const d = await call<string>("set_local_reference");
    toast(`Local reference set: ${d}`);
    await refreshLocalRef();
  } catch (e) {
    toast((e as Error).message, true);
  }
});
$("btn-pickref").addEventListener("click", async () => {
  try {
    const d = await call<string | null>("pick_reference_file");
    if (d) {
      toast(`Local reference loaded: ${d}`);
      await refreshLocalRef();
    }
  } catch (e) {
    toast((e as Error).message, true);
  }
});

function showScan(v: ScanView) {
  state.view = v;
  if ($<HTMLInputElement>("plot-overlay").checked) state.history.push(v);
  else state.history = [v];
  if (state.history.length > 8) state.history.shift();
  $<HTMLButtonElement>("btn-save").disabled = false;
  $<HTMLButtonElement>("btn-setref").disabled = false;
  renderMeta(v);
  drawPlot();
}

function renderMeta(v: ScanView) {
  const m = v.meta;
  const items: [string, string][] = [
    ["Scan", m.scan_name || "–"],
    ["Time", m.timestamp],
    ["Config", m.config_name],
    ["Points", String(m.length)],
    ["Averaged", `${m.num_repeats}×`],
    ["PGA", String(m.pga)],
    ["System temp (HDC1000)", `${m.system_temp_c.toFixed(2)} °C`],
    ["Detector temp (TMP006)", m.detector_temp_c === 0 ? "n/a (TMP006 not fitted)" : `${m.detector_temp_c.toFixed(2)} °C`],
    ["Humidity", `${m.humidity_pct.toFixed(2)} %`],
    ["Lamp PD", String(m.lamp_pd)],
    ["Serial", m.serial_number],
  ];
  m.sections.forEach((s, i) =>
    items.push([`Section ${i + 1}`, `${s.scan_type} ${s.start_nm}–${s.end_nm} nm, ${s.width_nm.toFixed(2)} nm, ${s.num_patterns} pts, ${s.exposure_us} µs`]),
  );
  $("scan-meta").innerHTML =
    items.map(([k, val]) => `<div><span>${esc(k)}</span>${esc(val)}</div>`).join("") +
    (v.reference_error ? `<div class="warn">Reference: ${esc(v.reference_error)}</div>` : "");
}

// ---------------------------------------------------------------- plot

let plot: uPlot | null = null;
const COLORS = ["#2f6fde", "#e0533d", "#2e9e5b", "#c98a12", "#8a4fd8", "#1aa3b8", "#d6489a", "#6b7385"];

document.querySelectorAll<HTMLButtonElement>("#plot-type button").forEach((b) =>
  b.addEventListener("click", () => {
    document.querySelectorAll("#plot-type button").forEach((x) => x.classList.remove("active"));
    b.classList.add("active");
    state.plotType = b.dataset.t as typeof state.plotType;
    drawPlot();
  }),
);
$("btn-clear-plot").addEventListener("click", () => {
  state.history = state.view ? [state.view] : [];
  drawPlot();
});

function seriesOf(v: ScanView): (number | null)[] | null {
  switch (state.plotType) {
    case "absorbance":
      return v.absorbance;
    case "reflectance":
      return v.reflectance;
    case "intensity":
      return v.intensity;
    case "reference":
      return v.reference;
  }
}

function drawPlot() {
  const el = $("plot");
  const msg = $("plot-msg");
  const views = state.history.filter((v) => seriesOf(v));
  if (!views.length) {
    plot?.destroy();
    plot = null;
    msg.textContent = state.view
      ? `No ${state.plotType} data${state.view.reference_error ? `: ${state.view.reference_error}` : ""}.`
      : "No scan yet. Connect the NIRscan Nano and press Scan.";
    msg.classList.remove("hidden");
    return;
  }
  msg.classList.add("hidden");
  // Merge x axes (scans may have different wavelengths)
  const xs = [...new Set(views.flatMap((v) => v.wavelength))].sort((a, b) => a - b);
  const data: (number | null)[][] = [xs];
  for (const v of views) {
    const map = new Map<number, number | null>();
    const ys = seriesOf(v)!;
    v.wavelength.forEach((w, i) => map.set(w, ys[i] == null || !Number.isFinite(ys[i] as number) ? null : (ys[i] as number)));
    data.push(xs.map((x) => (map.has(x) ? map.get(x)! : null)));
  }
  const yLabel = { absorbance: "Absorbance (AU)", reflectance: "Reflectance", intensity: "Intensity (counts)", reference: "Reference (counts)" }[state.plotType];
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  const axisColor = dark ? "#9aa1b0" : "#6b7385";
  const grid = { stroke: dark ? "#2c3038" : "#e6e8ee", width: 1 };
  const opts: uPlot.Options = {
    width: el.clientWidth,
    height: Math.max(220, el.clientHeight - 56),
    scales: { x: { time: false } },
    axes: [
      { label: "Wavelength (nm)", stroke: axisColor, grid },
      { label: yLabel, stroke: axisColor, grid, size: 70 },
    ],
    series: [
      { label: "nm", value: (_u, v) => (v == null ? "–" : v.toFixed(2)) },
      ...views.map((v, i) => ({
        label: `${v.meta.scan_name || "scan"} ${v.meta.timestamp.slice(11)}`,
        stroke: COLORS[i % COLORS.length],
        width: 1.6,
        spanGaps: false,
        value: (_u: uPlot, y: number | null) => (y == null ? "–" : state.plotType === "absorbance" || state.plotType === "reflectance" ? y.toFixed(4) : y.toFixed(0)),
      })),
    ],
    cursor: { drag: { x: true, y: true } },
  };
  plot?.destroy();
  plot = new uPlot(opts, data as uPlot.AlignedData, el);
}

function resizePlot() {
  if (plot) plot.setSize({ width: $("plot").clientWidth, height: Math.max(220, $("plot").clientHeight - 56) });
}
new ResizeObserver(resizePlot).observe($("plot"));

// ---------------------------------------------------------------- start

renderConn();
refreshLocalRef();
poll();
setInterval(poll, 2000);
