/* ============================================================
   RustAgent Web UI 前端逻辑
   约定: 接口全部挂在 /api/* 下; 聊天为 NDJSON 流式返回,
   新增事件类型只需在 handleEvent 里加一个 case。
   ============================================================ */

const $ = (id) => document.getElementById(id);
const DOM = Object.fromEntries(
  [
    "messages", "input", "btn-send", "btn-stop", "stat-usage", "stat-turn", "stat-db",
    "session-file", "session-list", "pack-dir", "pack-list", "tag-weights",
    "tool-list", "model-name", "ver", "status-dot", "status-text",
    "sel-version", "sel-loader", "sel-limit", "input-limit", "custom-limit-wrap",
    "btn-rec", "rec-list", "rec-hint",
  ].map((id) => [id, $(id)])
);

const state = {
  busy: false,
  presetKey: "rustagent-preset",
  limitCap: 20,
};

const API = {
  request: async (path, options = {}) => {
    const response = await fetch(path, options);
    const body = await response.text();
    let data;
    try { data = body ? JSON.parse(body) : {}; } catch { data = { message: body }; }
    if (!response.ok) throw new Error(data.message || `HTTP ${response.status}`);
    return data;
  },
  get: (path) => API.request(path),
  post: (path, body) =>
    API.request(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: body ? JSON.stringify(body) : undefined,
    }),
};

/* ---------- Toast ---------- */
let toastTimer = null;
function toast(msg, isErr = false) {
  const el = document.getElementById("toast");
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
  const m = DOM.messages;
  m.scrollTop = m.scrollHeight;
}

function addMsg(cls, html) {
  removeWelcome();
  // 消息行 = 头像 + 气泡; 返回气泡本身 (流式 reply_delta 直接改 textContent)
  const row = document.createElement("div");
  row.className = "msg-row " + cls;
  const avatar = document.createElement("div");
  avatar.className = "avatar";
  avatar.textContent = cls === "user" ? "我" : cls === "error" ? "!" : "⛏";
  const div = document.createElement("div");
  div.className = "msg " + cls;
  div.innerHTML = html;
  row.append(avatar, div);
  DOM.messages.appendChild(row);
  scrollBottom();
  return div;
}

function addToolLine(name) {
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line pending";
  div.textContent = `⚙ 调用工具 ${name} …`;
  DOM.messages.appendChild(div);
  scrollBottom();
  return div;
}

// 实时进展行: 同一次工具调用内的多条 progress 事件原地更新同一行
function addProgressLine() {
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line progress";
  DOM.messages.appendChild(div);
  return div;
}

// 数值进展渲染 ▰▱ 进度条 (progress 事件带 current/total 时)
function progressPrefix(ev) {
  if (!ev.total) return "";
  const w = 20;
  const filled = Math.round(Math.min(ev.current || 0, ev.total) / ev.total * w);
  return "▰".repeat(filled) + "▱".repeat(w - filled) + ` ${ev.current}/${ev.total} `;
}

function clearProgress(progress) {
  if (progress.el) {
    progress.el.remove();
    progress.el = null;
  }
}

// "思考中"提示: 覆盖等待 LLM 响应的空窗期 (发出消息后 / 工具结果返回后),
// 有任何事件到达即消失, 避免用户以为卡死; 附每秒递增的已等待时长
function showThinking(thinking) {
  if (thinking.el) return;
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line progress thinking";
  div.textContent = "思考中";
  DOM.messages.appendChild(div);
  scrollBottom();
  thinking.el = div;
  const start = Date.now();
  thinking.timer = setInterval(() => {
    if (thinking.el) {
      thinking.el.textContent = `思考中 (${Math.round((Date.now() - start) / 1000)}s)`;
    }
  }, 1000);
}

function hideThinking(thinking) {
  if (thinking.timer) {
    clearInterval(thinking.timer);
    thinking.timer = null;
  }
  if (thinking.el) {
    thinking.el.remove();
    thinking.el = null;
  }
}

// 欢迎页快捷示例: 点击填入输入框 (不自动发送, 用户可改)
const EXAMPLE_PROMPTS = [
  "帮我组一个 1.20.1 fabric 的探索向整合包",
  "找几个提升性能的优化 mod",
  "1.21.1 neoforge 魔法题材整合包",
  "推荐一些好看的视觉增强模组",
];

function showWelcome() {
  const div = document.createElement("div");
  div.className = "welcome";
  const logo = document.createElement("div");
  logo.className = "welcome-logo";
  logo.textContent = "⛏";
  const h = document.createElement("h2");
  h.textContent = "今天想玩点什么?";
  const p = document.createElement("p");
  p.textContent = "描述你的想法, 我来检索 mod、检查兼容性并打包成可直接拖进启动器的 .mrpack";
  const chips = document.createElement("div");
  chips.className = "welcome-chips";
  for (const ex of EXAMPLE_PROMPTS) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "chip";
    b.textContent = ex;
    b.addEventListener("click", () => {
      DOM.input.value = ex;
      autoGrow();
      DOM.input.focus();
    });
    chips.appendChild(b);
  }
  div.append(logo, h, p, chips);
  DOM.messages.appendChild(div);
}

function removeWelcome() {
  const w = document.querySelector(".welcome");
  if (w) w.remove();
}

/* ---------- 侧栏数据加载 ---------- */
async function loadInfo() {
  try {
    const info = await API.get("/api/info");
    DOM["model-name"].textContent = `模型: ${info.model}`;
    DOM.ver.textContent = "v" + info.version;
    DOM["stat-usage"].textContent =
      `${info.calls} 次调用 · ${info.total_tokens} tokens · ¥${info.cost}`;
    DOM["session-file"].textContent = info.session_file
      ? "自动保存: " + info.session_file
      : "自动保存: 尚无对话";
  } catch { /* 服务器未就绪时静默 */ }
}

async function loadProfile() {
  try {
    const p = await API.get("/api/profile");
    DOM["stat-db"].textContent = p.summary;
    const tags = DOM["tag-weights"];
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
    const ul = DOM["tool-list"];
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
    DOM["pack-dir"].textContent = "目录: " + p.dir;
    const ul = DOM["pack-list"];
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
  return Promise.allSettled([loadInfo(), loadProfile(), loadPacks(), loadSessions()]);
}

// 下载量格式化: 12.3M / 5.6K
function fmtDownloads(n) {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "K";
  return String(n);
}

/* ---------- "试试这个" 推荐 (不持 agent 锁, 与对话并行) ---------- */
async function loadRecommend() {
  const gv = DOM["sel-version"].value.trim();
  if (!gv) {
    DOM["rec-hint"].textContent = "请先在预设栏选择 MC 版本";
    DOM["rec-hint"].classList.remove("hidden");
    DOM["rec-list"].innerHTML = "";
    return;
  }
  DOM["rec-hint"].textContent = "拉取中…";
  DOM["rec-hint"].classList.remove("hidden");
  DOM["rec-list"].innerHTML = "";
  try {
    const r = await API.get(
      `/api/recommend?game_version=${encodeURIComponent(gv)}&loader=${encodeURIComponent(DOM["sel-loader"].value)}`
    );
    if (!r.ok) {
      DOM["rec-hint"].textContent = r.message || "拉取失败";
      return;
    }
    DOM["rec-hint"].classList.add("hidden");
    const ul = DOM["rec-list"];
    ul.innerHTML = "";
    for (const m of r.recommendations) {
      const li = document.createElement("li");
      li.className = "rec-item";
      li.dataset.slug = m.slug;
      li.innerHTML =
        `<div class="rec-name">${escapeHtml(m.title)} <span class="pmeta">${escapeHtml(m.slug)}</span></div>` +
        `<div class="rec-desc">${escapeHtml(m.description)}</div>` +
        `<div class="rec-meta">${fmtDownloads(m.downloads)} 下载` +
        (m.categories?.length ? ` · ${escapeHtml(m.categories.join(", "))}` : "") +
        ` · <span class="tscore">口味 ${m.taste_score.toFixed(1)}</span></div>`;
      const btns = document.createElement("div");
      btns.className = "rec-btns";
      const likeBtn = document.createElement("button");
      likeBtn.textContent = "👍";
      likeBtn.onclick = () => rateRec(m.slug, "like", li, likeBtn);
      const dislikeBtn = document.createElement("button");
      dislikeBtn.textContent = "👎";
      dislikeBtn.onclick = () => rateRec(m.slug, "dislike", li, dislikeBtn);
      btns.append(likeBtn, dislikeBtn);
      li.appendChild(btns);
      ul.appendChild(li);
    }
  } catch {
    DOM["rec-hint"].textContent = "请求失败";
  }
}

// 点 👍/👎 即写库 (不经过 LLM, 不阻塞对话), 成功后标记该项并刷新统计
async function rateRec(slug, verdict, item, btn) {
  try {
    const r = await API.post("/api/feedback", { slug, verdict });
    toast(r.ok ? `已记录 ${verdict === "like" ? "喜欢" : "不喜欢"} ${slug}` : (r.message || "失败"), !r.ok);
    if (!r.ok) return;
    item.classList.add("rated");
    // 该项两个按钮都禁用, 当前操作高亮
    item.querySelectorAll(".rec-btns button").forEach((b) => (b.disabled = true));
    btn.classList.add(verdict === "like" ? "liked" : "disliked");
    // 反馈改变了口味数据, 刷新统计与标签权重
    loadProfile();
    loadInfo();
  } catch {
    toast("请求失败", true);
  }
}

/* ---------- 聊天 (NDJSON 流式) ---------- */
function setBusy(b) {
  state.busy = b;
  DOM["btn-send"].disabled = b;
  // 忙碌时显示打断按钮 (长任务/长思考时可中止当前轮)
  DOM["btn-stop"].classList.toggle("hidden", !b);
}

async function send() {
  const input = DOM.input;
  const text = input.value.trim();
  if (!text || state.busy) return;
  input.value = "";
  autoGrow();
  setBusy(true);

  addMsg("user", escapeHtml(text));
  const pending = []; // { name, el, done }; replyEl 属性 = 当前流式回复气泡 (本轮共用)
  const progress = { el: null }; // 当前实时进展行 (整个 turn 共用一个, 原地更新)
  const thinking = { el: null, timer: null }; // "思考中"行状态 + 每秒计时器
  pending.replyEl = null;
  showThinking(thinking);

  try {
    const res = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        text,
        game_version: DOM["sel-version"].value.trim() || undefined,
        loader: DOM["sel-loader"].value || undefined,
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
    DOM.input.focus();
  }
}

function handleEvent(ev, pending, progress, thinking) {
  switch (ev.type) {
    case "tool_call":
      hideThinking(thinking);
      // 结束当前回复气泡: 工具调用后的新文本应出现在工具行下方,
      // 而不是续写到工具行上方的旧气泡里 (否则多轮工具调用时回复会把工具行夹在中间)
      pending.replyEl = null;
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
      progress.el.textContent = `⏳ ${progressPrefix(ev)}${ev.text}`;
      scrollBottom();
      break;
    case "reply_delta": {
      // 流式打字机: 增量阶段用纯文本追加, 完整 reply 到达后用 markdown 重渲染
      hideThinking(thinking);
      clearProgress(progress);
      if (!pending.replyEl) pending.replyEl = addMsg("assistant", "");
      pending.replyEl.textContent += ev.text;
      scrollBottom();
      break;
    }
    case "reply":
      hideThinking(thinking);
      clearProgress(progress);
      if (pending.replyEl) {
        pending.replyEl.innerHTML = renderText(ev.text);
        pending.replyEl = null;
      } else {
        addMsg("assistant", renderText(ev.text));
      }
      break;
    case "error":
      hideThinking(thinking);
      clearProgress(progress);
      addMsg("error", escapeHtml(ev.message));
      break;
    case "llm_usage":
      DOM["stat-turn"].textContent =
        `上次调用 · 输入 ${ev.prompt_tokens} · 输出 ${ev.completion_tokens} · 合计 ${ev.total_tokens} tok`;
      break;
    case "done":
      hideThinking(thinking);
      clearProgress(progress);
      DOM["stat-usage"].textContent = ev.usage;
      if (ev.saved) loadSessions(); // 本轮已自动保存, 立即刷新会话列表
      break;
  }
}

/* ---------- 会话管理 ---------- */
async function session(action) {
  if (state.busy && (action === "new" || action === "import")) {
    toast("请等待本轮对话结束", true);
    return;
  }
  try {
    const r = await API.post(`/api/session/${action}`);
    toast(r.message, !r.ok);
    if (r.ok && Array.isArray(r.messages)) renderHistory(r.messages);
    if (action === "new" && r.ok) {
      DOM.messages.innerHTML = "";
      showWelcome();
      refreshSidebar();
    }
  } catch {
    toast("请求失败", true);
  }
}

// 导入指定会话文件 (点击会话列表条目触发)
async function importSession(name) {
  if (state.busy) {
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
  DOM.messages.innerHTML = "";
  for (const m of messages) {
    if (m.kind === "user") {
      addMsg("user", escapeHtml(m.text));
    } else if (m.kind === "assistant") {
      addMsg("assistant", renderText(m.text));
    } else {
  const div = document.createElement("div");
      div.className = "tool-line ok";
      div.textContent = "⚙ " + m.text;
  DOM.messages.appendChild(div);
    }
  }
  if (!messages.length) showWelcome();
  scrollBottom();
}

// 会话记录列表 (点击合法条目即导入并渲染, 与整合包卡片同款交互)
async function loadSessions() {
  try {
    const s = await API.get("/api/sessions");
    const ul = DOM["session-list"];
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

/* ---------- 设置窗口 (运行时改 LLM 配置, 保存后热生效并写回 config.toml) ---------- */
async function openSettings() {
  $("settings-modal").classList.remove("hidden");
  try {
    const s = await API.get("/api/settings");
    $("set-model").value = s.model;
    $("set-base-url").value = s.base_url;
    $("set-api-key").value = "";
    $("set-key-hint").textContent = s.api_key_set
      ? `留空保持不变 (已设置 ${s.api_key_masked})`
      : "尚未设置";
    $("set-ctx").value = s.context_length;
    $("set-budget").value = s.token_budget;
    $("set-max-tools").value = s.max_tool_iterations;
    $("set-price-in").value = s.price_input_per_m;
    $("set-price-out").value = s.price_output_per_m;
    $("set-thinking").value = s.thinking || "";
  } catch {
    toast("读取设置失败", true);
  }
}

function closeSettings() {
  $("settings-modal").classList.add("hidden");
}

async function saveSettings(e) {
  e.preventDefault();
  if (state.busy) {
    toast("请等待本轮对话结束再修改设置", true);
    return;
  }
  const body = {
    model: $("set-model").value.trim(),
    base_url: $("set-base-url").value.trim(),
    context_length: parseInt($("set-ctx").value, 10),
    token_budget: parseInt($("set-budget").value, 10),
    max_tool_iterations: parseInt($("set-max-tools").value, 10),
    price_input_per_m: parseFloat($("set-price-in").value),
    price_output_per_m: parseFloat($("set-price-out").value),
    thinking: $("set-thinking").value.trim(),
  };
  const key = $("set-api-key").value.trim();
  if (key) body.api_key = key;
  // 空数字字段不发送 (保持原值), 避免 NaN 序列化成 null
  for (const k of ["context_length", "token_budget", "max_tool_iterations", "price_input_per_m", "price_output_per_m"]) {
    if (!Number.isFinite(body[k])) delete body[k];
  }
  try {
    const r = await API.post("/api/settings", body);
    toast(r.message, !r.ok);
    if (r.ok) {
      closeSettings();
      loadInfo(); // 顶栏模型名随之刷新
    }
  } catch {
    toast("请求失败", true);
  }
}

/* ---------- 健康检查 ---------- */
async function checkHealth() {
  const dot = DOM["status-dot"];
  const txt = DOM["status-text"];
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
// Persistent controls are kept in one state object.

// 生效的找包数量: 预设档位直取, 自定义档位钳制到 1..=20
function currentLimit() {
  const v = $("sel-limit").value;
  if (v !== "custom") return parseInt(v, 10);
  const n = parseInt($("input-limit").value, 10);
  if (!n || n < 1) return undefined;
  return Math.min(n, state.limitCap);
}

// 自定义档位时才显示数字输入框和上限提醒
function updateCustomLimit() {
  const custom = $("sel-limit").value === "custom";
  $("custom-limit-wrap").classList.toggle("hidden", !custom);
}

function savePreset() {
  localStorage.setItem(state.presetKey, JSON.stringify({
    game_version: $("sel-version").value.trim(),
    loader: $("sel-loader").value,
    search_limit: $("sel-limit").value,
    search_limit_custom: $("input-limit").value,
  }));
}

function loadPreset() {
  try {
    const p = JSON.parse(localStorage.getItem(state.presetKey) || "{}");
    if (p.game_version) $("sel-version").value = p.game_version;
    if (p.loader) $("sel-loader").value = p.loader;
    if (p.search_limit) $("sel-limit").value = p.search_limit;
    if (p.search_limit_custom) $("input-limit").value = p.search_limit_custom;
  } catch { /* 忽略损坏的历史数据 */ }
  updateCustomLimit();
  syncLoaderSeg(); // 隐藏 input 恢复值后, 分段按钮高亮态跟随
}

// 加载器分段控件: 隐藏 input #sel-loader 是唯一数据源 (send/loadRecommend 读它),
// 按钮只是视图, 点选与恢复预设后都调本函数同步高亮
function syncLoaderSeg() {
  const v = $("sel-loader").value;
  $("seg-loader").querySelectorAll(".seg-btn").forEach((b) => {
    b.classList.toggle("active", b.dataset.value === v);
  });
}

/* ---------- 输入框 ---------- */
function autoGrow() {
  const input = $("input");
  input.style.height = "auto";
  input.style.height = Math.min(input.scrollHeight, 160) + "px";
}

/* ---------- 事件绑定与启动 ---------- */
DOM.input.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    send();
  }
});
DOM.input.addEventListener("input", autoGrow);
$("btn-send").addEventListener("click", send);
$("inputbar").addEventListener("submit", (e) => { e.preventDefault(); send(); });
$("presetbar").addEventListener("submit", (e) => e.preventDefault());
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
$("seg-loader").querySelectorAll(".seg-btn").forEach((b) => {
  b.addEventListener("click", () => {
    $("sel-loader").value = b.dataset.value;
    syncLoaderSeg();
    savePreset();
  });
});
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
$("btn-rec").addEventListener("click", loadRecommend);
$("btn-settings").addEventListener("click", openSettings);
$("btn-settings-close").addEventListener("click", closeSettings);
$("btn-settings-cancel").addEventListener("click", closeSettings);
$("settings-mask").addEventListener("click", closeSettings);
$("settings-form").addEventListener("submit", saveSettings);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeSettings();
});

loadPreset();
showWelcome();
checkHealth();
refreshSidebar();
loadTools();
loadRecommend();
DOM.input.focus();
