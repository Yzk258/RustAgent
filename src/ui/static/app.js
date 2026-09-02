/* ============================================================
   RustAgent Web UI 前端逻辑
   约定: 接口全部挂在 /api/* 下; 聊天为 NDJSON 流式返回,
   新增事件类型只需在 handleEvent 里加一个 case。
   ============================================================ */

const $ = (id) => document.getElementById(id);

const API = {
  get: (path) => fetch(path).then((r) => r.json()),
  post: (path, body) =>
    fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: body ? JSON.stringify(body) : undefined,
    }).then((r) => r.json()),
};

/* ---------- Toast ---------- */
let toastTimer = null;
function toast(msg, isErr = false) {
  const el = $("toast");
  el.textContent = msg;
  el.classList.toggle("err", isErr);
  el.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add("hidden"), 3000);
}

/* ---------- 消息渲染 ---------- */
function escapeHtml(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

// 轻量 markdown: **加粗** 与 `行内代码`
function renderText(s) {
  return escapeHtml(s)
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/`([^`]+)`/g, "<code>$1</code>");
}

function scrollBottom() {
  const m = $("messages");
  m.scrollTop = m.scrollHeight;
}

function addMsg(cls, html) {
  removeWelcome();
  const div = document.createElement("div");
  div.className = "msg " + cls;
  div.innerHTML = html;
  $("messages").appendChild(div);
  scrollBottom();
  return div;
}

function addToolLine(name) {
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line pending";
  div.textContent = `⚙ 调用工具 ${name} …`;
  $("messages").appendChild(div);
  scrollBottom();
  return div;
}

// 实时进展行: 同一次工具调用内的多条 progress 事件原地更新同一行
function addProgressLine() {
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line progress";
  $("messages").appendChild(div);
  return div;
}

function clearProgress(progress) {
  if (progress.el) {
    progress.el.remove();
    progress.el = null;
  }
}

// "思考中"提示: 覆盖等待 LLM 响应的空窗期 (发出消息后 / 工具结果返回后),
// 有任何事件到达即消失, 避免用户以为卡死
function showThinking(thinking) {
  if (thinking.el) return;
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line progress thinking";
  div.textContent = "思考中";
  $("messages").appendChild(div);
  scrollBottom();
  thinking.el = div;
}

function hideThinking(thinking) {
  if (thinking.el) {
    thinking.el.remove();
    thinking.el = null;
  }
}

function showWelcome() {
  const div = document.createElement("div");
  div.className = "welcome";
  div.innerHTML =
    "<b>⛏ RustAgent</b><br>描述你想要的整合包, 例如:<br>“帮我组一个 1.20.1 fabric 的探索向整合包”";
  $("messages").appendChild(div);
}

function removeWelcome() {
  const w = document.querySelector(".welcome");
  if (w) w.remove();
}

/* ---------- 侧栏数据加载 ---------- */
async function loadInfo() {
  try {
    const info = await API.get("/api/info");
    $("model-name").textContent = `模型: ${info.model}`;
    $("ver").textContent = "v" + info.version;
    $("stat-usage").textContent =
      `${info.calls} 次调用 · ${info.total_tokens} tokens · ¥${info.cost}`;
  } catch { /* 服务器未就绪时静默 */ }
}

async function loadProfile() {
  try {
    const p = await API.get("/api/profile");
    $("stat-db").textContent = p.summary;
    const tags = $("tag-weights");
    tags.innerHTML = "";
    for (const { tag, weight } of p.tag_weights) {
      const chip = document.createElement("span");
      chip.className = "tag-chip" + (weight < 0 ? " negative" : "");
      chip.textContent = `${tag} ${weight >= 0 ? "+" : ""}${weight.toFixed(1)}`;
      tags.appendChild(chip);
    }
  } catch { /* ignore */ }
}

async function loadTools() {
  try {
    const t = await API.get("/api/tools");
    const ul = $("tool-list");
    ul.innerHTML = "";
    for (const tool of t.tools) {
      const li = document.createElement("li");
      li.innerHTML =
        `<span class="tname">${escapeHtml(tool.name)}</span><br>` +
        `<span class="tdesc">${escapeHtml(tool.description)}</span>`;
      ul.appendChild(li);
    }
  } catch { /* ignore */ }
}

async function loadPacks() {
  try {
    const p = await API.get("/api/packs");
    $("pack-dir").textContent = "目录: " + p.dir;
    const ul = $("pack-list");
    ul.innerHTML = "";
    if (!p.packs.length) {
      const li = document.createElement("li");
      li.innerHTML = '<span class="pmeta">还没有生成过整合包</span>';
      ul.appendChild(li);
      return;
    }
    for (const pk of p.packs) {
      const li = document.createElement("li");
      li.innerHTML =
        `<div class="pname">${escapeHtml(pk.name)}</div>` +
        `<div class="pmeta">${pk.size_kb} KB · ${escapeHtml(pk.modified)}</div>`;
      ul.appendChild(li);
    }
  } catch { /* ignore */ }
}

function refreshSidebar() {
  loadInfo();
  loadProfile();
  loadPacks();
  loadSessions();
}

/* ---------- 聊天 (NDJSON 流式) ---------- */
let busy = false;

function setBusy(b) {
  busy = b;
  $("btn-send").disabled = b;
  // 忙碌时显示打断按钮 (长任务/长思考时可中止当前轮)
  $("btn-stop").classList.toggle("hidden", !b);
}

async function send() {
  const input = $("input");
  const text = input.value.trim();
  if (!text || busy) return;
  input.value = "";
  autoGrow();
  setBusy(true);

  addMsg("user", escapeHtml(text));
  const pending = []; // { name, el, done }
  const progress = { el: null }; // 当前实时进展行 (整个 turn 共用一个, 原地更新)
  const thinking = { el: null }; // "思考中"提示行状态
  showThinking(thinking);

  try {
    const res = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        text,
        game_version: $("sel-version").value.trim() || undefined,
        loader: $("sel-loader").value || undefined,
        search_limit: currentLimit() || undefined,
      }),
    });
    if (!res.ok || !res.body) {
      let detail = "";
      try {
        detail = (await res.text()).slice(0, 200);
      } catch { /* 忽略读取失败 */ }
      throw new Error(`HTTP ${res.status} ${detail}`);
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx).trim();
        buf = buf.slice(idx + 1);
        if (line) handleEvent(JSON.parse(line), pending, progress, thinking);
      }
    }
  } catch (e) {
    hideThinking(thinking);
    addMsg("error", "连接失败: " + escapeHtml(e.message || e));
  } finally {
    hideThinking(thinking);
    setBusy(false);
    refreshSidebar();
    $("input").focus();
  }
}

function handleEvent(ev, pending, progress, thinking) {
  switch (ev.type) {
    case "tool_call":
      hideThinking(thinking);
      pending.push({ name: ev.name, el: addToolLine(ev.name), done: false });
      break;
    case "tool_result": {
      hideThinking(thinking);
      const t = pending.find((x) => x.name === ev.name && !x.done);
      if (t) {
        t.done = true;
        t.el.classList.remove("pending");
        t.el.classList.add(ev.ok ? "ok" : "fail");
        t.el.textContent = `⚙ ${ev.name} ${ev.ok ? "完成" : "失败"}`;
      }
      clearProgress(progress);
      // 工具结果要再交给 LLM 分析, 又进入思考空窗期
      showThinking(thinking);
      break;
    }
    case "progress":
      hideThinking(thinking);
      if (!progress.el) progress.el = addProgressLine();
      progress.el.textContent = `⏳ ${ev.text}`;
      scrollBottom();
      break;
    case "reply":
      hideThinking(thinking);
      clearProgress(progress);
      addMsg("assistant", renderText(ev.text));
      break;
    case "error":
      hideThinking(thinking);
      clearProgress(progress);
      addMsg("error", escapeHtml(ev.message));
      break;
    case "done":
      hideThinking(thinking);
      clearProgress(progress);
      $("stat-usage").textContent = ev.usage;
      break;
  }
}

/* ---------- 会话管理 ---------- */
async function session(action) {
  if (busy && (action === "new" || action === "import")) {
    toast("请等待本轮对话结束", true);
    return;
  }
  try {
    const r = await API.post(`/api/session/${action}`);
    toast(r.message, !r.ok);
    if (r.ok && Array.isArray(r.messages)) renderHistory(r.messages);
    if (action === "new" && r.ok) {
      $("messages").innerHTML = "";
      showWelcome();
      refreshSidebar();
    }
  } catch {
    toast("请求失败", true);
  }
}

// 导入指定会话文件 (点击会话列表条目触发)
async function importSession(name) {
  if (busy) {
    toast("请等待本轮对话结束", true);
    return;
  }
  try {
    const r = await API.post("/api/session/import", { name });
    toast(r.message, !r.ok);
    if (r.ok && Array.isArray(r.messages)) {
      renderHistory(r.messages);
      loadInfo();
    }
  } catch {
    toast("请求失败", true);
  }
}

// 把导入/加载的历史消息渲染到聊天区
function renderHistory(messages) {
  $("messages").innerHTML = "";
  for (const m of messages) {
    if (m.kind === "user") {
      addMsg("user", escapeHtml(m.text));
    } else if (m.kind === "assistant") {
      addMsg("assistant", renderText(m.text));
    } else {
      const div = document.createElement("div");
      div.className = "tool-line ok";
      div.textContent = "⚙ " + m.text;
      $("messages").appendChild(div);
    }
  }
  if (!messages.length) showWelcome();
  scrollBottom();
}

// 会话记录列表 (点击合法条目即导入并渲染, 与整合包卡片同款交互)
async function loadSessions() {
  try {
    const s = await API.get("/api/sessions");
    const ul = $("session-list");
    ul.innerHTML = "";
    if (!s.sessions.length) {
      ul.innerHTML = '<li><span class="pmeta">暂无会话记录</span></li>';
      return;
    }
    for (const it of s.sessions) {
      const li = document.createElement("li");
      li.className = it.valid ? "clickable" : "invalid";
      li.title = it.valid ? "点击导入并渲染该会话" : "非法会话文件, 不可导入";
      li.innerHTML =
        `<div class="pname">${escapeHtml(it.name)}</div>` +
        `<div class="pmeta">${it.size_kb} KB · ${escapeHtml(it.modified)}${it.valid ? "" : " · 非法"}</div>`;
      if (it.valid) li.addEventListener("click", () => importSession(it.name));
      ul.appendChild(li);
    }
  } catch { /* ignore */ }
}

/* ---------- 健康检查 ---------- */
async function checkHealth() {
  const dot = $("status-dot");
  const txt = $("status-text");
  try {
    await API.get("/api/health");
    dot.className = "dot ok";
    txt.textContent = "已连接";
  } catch {
    dot.className = "dot bad";
    txt.textContent = "离线";
  }
}

/* ---------- 预设选项栏 (版本/加载器/找包数量) ---------- */
const PRESET_KEY = "rustagent-preset";
const LIMIT_CAP = 20; // 单次对话找包数量上限

// 生效的找包数量: 预设档位直取, 自定义档位钳制到 1..=20
function currentLimit() {
  const v = $("sel-limit").value;
  if (v !== "custom") return parseInt(v, 10);
  const n = parseInt($("input-limit").value, 10);
  if (!n || n < 1) return undefined;
  return Math.min(n, LIMIT_CAP);
}

// 自定义档位时才显示数字输入框和上限提醒
function updateCustomLimit() {
  const custom = $("sel-limit").value === "custom";
  $("custom-limit-wrap").classList.toggle("hidden", !custom);
}

function savePreset() {
  localStorage.setItem(PRESET_KEY, JSON.stringify({
    game_version: $("sel-version").value.trim(),
    loader: $("sel-loader").value,
    search_limit: $("sel-limit").value,
    search_limit_custom: $("input-limit").value,
  }));
}

function loadPreset() {
  try {
    const p = JSON.parse(localStorage.getItem(PRESET_KEY) || "{}");
    if (p.game_version) $("sel-version").value = p.game_version;
    if (p.loader) $("sel-loader").value = p.loader;
    if (p.search_limit) $("sel-limit").value = p.search_limit;
    if (p.search_limit_custom) $("input-limit").value = p.search_limit_custom;
  } catch { /* 忽略损坏的历史数据 */ }
  updateCustomLimit();
}

/* ---------- 输入框 ---------- */
function autoGrow() {
  const input = $("input");
  input.style.height = "auto";
  input.style.height = Math.min(input.scrollHeight, 160) + "px";
}

/* ---------- 事件绑定与启动 ---------- */
$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    send();
  }
});
$("input").addEventListener("input", autoGrow);
$("btn-send").addEventListener("click", send);
$("btn-stop").addEventListener("click", async () => {
  try {
    const r = await API.post("/api/chat/interrupt");
    toast(r.message, !r.ok);
  } catch {
    toast("请求失败", true);
  }
});
$("btn-new").addEventListener("click", () => session("new"));
$("btn-save").addEventListener("click", () => session("save"));
$("btn-load").addEventListener("click", () => session("load"));
$("sel-version").addEventListener("change", savePreset);
$("sel-loader").addEventListener("change", savePreset);
$("sel-limit").addEventListener("change", () => {
  updateCustomLimit();
  savePreset();
});
$("input-limit").addEventListener("input", savePreset);
$("btn-open-sessions").addEventListener("click", async () => {
  try {
    const r = await API.post("/api/session/open");
    toast(r.message, !r.ok);
    if (r.ok) loadSessions();
  } catch {
    toast("请求失败", true);
  }
});
$("btn-open-dir").addEventListener("click", async () => {
  try {
    const r = await API.post("/api/packs/open");
    toast(r.message, !r.ok);
    if (r.ok) loadPacks();
  } catch {
    toast("请求失败", true);
  }
});

loadPreset();
showWelcome();
checkHealth();
refreshSidebar();
loadTools();
$("input").focus();
