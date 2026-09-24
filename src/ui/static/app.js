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
    "btn-rec", "rec-list", "rec-hint", "trial-hint",
  ].map((id) => [id, $(id)])
);

const state = {
  busy: false,
  presetKey: "rustagent-preset",
  limitCap: 20,
  trialTool: null, // 试用模式: 当前选定的工具名 (null = 普通对话)
};

// 设置窗口字段 DOM 缓存: openSettings/saveSettings 反复用 $("set-xxx") 查询,
// 缓存一次避免每次 open/save 都走 getElementById (设置窗口频繁开关)。
const SET = Object.fromEntries(
  [
    "set-model", "set-base-url", "set-api-key", "set-key-hint",
    "set-ctx", "set-budget", "set-max-tools", "set-price-in", "set-price-out",
    "set-thinking", "set-cf",
  ].map((id) => [id, $(id)])
);

// 当前对话轮的 fetch 控制器: 切换会话时用它丢弃旧轮的流式输出
let chatController = null;

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

function applyTheme(theme) {
  const light = theme === "light";
  const next = light ? "light" : "dark";
  const button = $("btn-theme");
  if (document.documentElement.dataset.theme === next && button.dataset.ready === "true") return;
  document.documentElement.dataset.theme = next;
  button.textContent = light ? "☾" : "☀";
  button.title = light ? "切换到黑夜模式" : "切换到白天模式";
  button.setAttribute("aria-label", button.title);
  $("theme-color").setAttribute("content", light ? "#f4f6fa" : "#0d0f14");
  button.dataset.ready = "true";
}

function toggleTheme() {
  const next = document.documentElement.dataset.theme === "light" ? "dark" : "light";
  localStorage.setItem("rustagent-theme", next);
  applyTheme(next);
}

/* ---------- 消息渲染 ---------- */
function escapeHtml(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

// markdown 渲染: marked 解析 (GFM 表格 + 单换行成 <br>), 再经 DOM 净化 ——
// 剥离原始 HTML 标签与非 http(s) 链接、危险属性, 防止 LLM 输出注入脚本。
// 属性用白名单 (而非黑名单删 on*): 只保留已知安全属性, 移除 style/javascript: 等
// —— style 可做 UI 欺骗 (position:fixed 覆盖整页), 黑名单会漏掉新出现的危险属性。
const MD_TAGS = new Set([
  "A", "B", "STRONG", "I", "EM", "S", "DEL", "CODE", "PRE", "UL", "OL", "LI",
  "BLOCKQUOTE", "H1", "H2", "H3", "H4", "H5", "H6", "P", "BR", "HR",
  "TABLE", "THEAD", "TBODY", "TFOOT", "TR", "TH", "TD", "SPAN", "INPUT",
]);
// 各标签允许保留的属性白名单 (其余一律移除)。INPUT 仅为 GFM task list 复选框保留
// disabled/checked/type, 防止被滥用构造可交互表单。
const MD_ATTRS = {
  A: new Set(["href", "title"]),
  INPUT: new Set(["type", "checked", "disabled"]),
  CODE: new Set(["class"]),
  SPAN: new Set(["class"]),
};

function renderText(s) {
  if (!window.marked) return escapeHtml(s); // 库加载失败时退回纯文本
  const div = document.createElement("div");
  div.innerHTML = marked.parse(String(s), { gfm: true, breaks: true });
  for (const el of [...div.querySelectorAll("*")]) {
    if (!MD_TAGS.has(el.tagName)) {
      el.replaceWith(document.createTextNode(el.textContent));
      continue;
    }
    // 属性白名单: 只保留该标签允许的属性, 移除 style/on*/javascript: 等一切危险属性
    const allowed = MD_ATTRS[el.tagName];
    for (const attr of [...el.attributes]) {
      if (!allowed || !allowed.has(attr.name.toLowerCase())) {
        el.removeAttribute(attr.name);
      }
    }
    if (el.tagName === "A") {
      if (/^https?:/i.test(el.getAttribute("href") || "")) {
        el.target = "_blank";
        el.rel = "noopener noreferrer";
      } else {
        el.replaceWith(...el.childNodes);
      }
    }
    if (el.tagName === "INPUT") {
      // GFM task list 复选框: 强制 disabled, 防止可交互 input 被滥用
      el.setAttribute("disabled", "");
    }
  }
  // 表格横向可滚动; 代码块挂复制按钮 (点击处理在 messages 上统一委托)
  for (const table of div.querySelectorAll("table")) {
    const wrap = document.createElement("div");
    wrap.className = "md-table-wrap";
    table.replaceWith(wrap);
    wrap.appendChild(table);
  }
  for (const pre of div.querySelectorAll("pre")) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "md-copy";
    btn.textContent = "复制";
    pre.appendChild(btn);
  }
  return div.innerHTML;
}

// 代码块复制: renderText 产出的按钮统一在此处理点击
DOM.messages.addEventListener("click", async (e) => {
  const btn = e.target.closest(".md-copy");
  if (!btn) return;
  const code = btn.parentElement.querySelector("code");
  try {
    await navigator.clipboard.writeText(code ? code.textContent : "");
    btn.textContent = "已复制";
  } catch {
    btn.textContent = "复制失败";
  }
  setTimeout(() => (btn.textContent = "复制"), 1500);
});

// 流式期间按帧合并重渲染 (每个 delta 都全量 parse 会浪费), 最终 reply 事件仍即时渲染。
// scrollBottom 也并入本帧, 避免每个 delta 各触发一次 scrollTop 赋值导致高频重绘。
// 自适应: 回复超 PARSE_LIMIT 字后切纯文本增量追加 (见 reply_delta), 避免长回复每帧
// 全量 parse 整段越来越慢 (组包报告常数千字, 后期每帧 parse 50ms+ 卡顿)。
const PARSE_LIMIT = 2000;
let mdPaintQueued = false;
let mdOverflowed = false; // 本轮回复是否已超阈值切纯文本模式
function queueMdRender(pending) {
  if (mdPaintQueued || !pending.replyEl) return;
  mdPaintQueued = true;
  requestAnimationFrame(() => {
    mdPaintQueued = false;
    if (!pending.replyEl || pending.replyText == null) return;
    if (mdOverflowed) {
      // 纯文本模式: 只追加增量, 不 parse (最终 reply 事件会全量渲染)
      pending.replyEl.textContent = pending.replyText;
    } else {
      pending.replyEl.innerHTML = renderText(pending.replyText);
      if (pending.replyText.length > PARSE_LIMIT) mdOverflowed = true;
    }
    scrollBottom();
  });
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
  div.className = "msg " + cls + (cls === "assistant" ? " md" : "");
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
// LLM 等待期轮换的阶段提示: 秒数之外给一点"在干什么"的活性信号
const THINKING_HINTS = [
  "理解需求中",
  "检索 mod 数据",
  "核对版本与加载器",
  "评估口味匹配",
  "整理回复",
];

// 思考行统一渲染: 推理型模型的实时思考片段优先 (💭 + 尾部片段), 否则轮换阶段提示
function renderThinking(thinking) {
  if (!thinking.el) return;
  const sec = Math.round((Date.now() - thinking.start) / 1000);
  if (thinking.tail) {
    const t = thinking.tail.replace(/\s+/g, " ").slice(-48);
    thinking.el.textContent = `💭 ${t} (${sec}s)`;
  } else {
    const hint = THINKING_HINTS[Math.floor(sec / 3) % THINKING_HINTS.length];
    thinking.el.textContent = `思考中 · ${hint}… (${sec}s)`;
  }
}

function showThinking(thinking) {
  if (thinking.el) return;
  thinking.tail = ""; // 新等待窗口: 清空上一段推理片段
  removeWelcome();
  const div = document.createElement("div");
  div.className = "tool-line progress thinking";
  div.textContent = "思考中";
  DOM.messages.appendChild(div);
  scrollBottom();
  thinking.el = div;
  thinking.start = Date.now();
  thinking.timer = setInterval(() => renderThinking(thinking), 1000);
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

// 可试用工具 (无副作用 + 参数可由预设栏推断); 其余工具不显示试用按钮
const TRIALABLE = new Set(["get_user_profile", "recommend_new_mods", "search_mods"]);

async function loadTools() {
  try {
    const t = await API.get("/api/tools");
    const ul = DOM["tool-list"];
    ul.innerHTML = "";
    for (const tool of t.tools) {
      const li = document.createElement("li");
      li.className = "tool-item";
      const info = document.createElement("div");
      info.innerHTML =
        `<span class="tname">${escapeHtml(tool.name)}</span><br>` +
        `<span class="tdesc">${escapeHtml(tool.description)}</span>`;
      li.appendChild(info);
      // 可试用的工具追加"试用"按钮, 点击进入试用模式
      if (TRIALABLE.has(tool.name)) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "tool-trial-btn";
        btn.textContent = "试用";
        btn.addEventListener("click", () => startTrial(tool.name));
        li.appendChild(btn);
      }
      ul.appendChild(li);
    }
  } catch { /* ignore */ }
}

// 试用模式: 选定工具后, 输入框上方提示条 + 输入框聚焦, 发送时走 /api/tool/trial
function startTrial(toolName) {
  state.trialTool = toolName;
  const hint = $("trial-hint");
  hint.querySelector(".trial-tool").textContent = toolName;
  hint.classList.remove("hidden");
  // 给输入框预填一个示例提示词, 引导用户
  const examples = {
    get_user_profile: "查看一下我的画像",
    recommend_new_mods: "给我推荐点新的",
    search_mods: "找点性能优化 mod",
  };
  DOM.input.value = examples[toolName] || "";
  autoGrow();
  DOM.input.focus();
  toast(`已进入试用模式: ${toolName}, 输入提示词后发送即触发演示`, false);
}

function exitTrial() {
  state.trialTool = null;
  $("trial-hint").classList.add("hidden");
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
      // 图标 (无 icon_url 时用占位 emoji), 标题可点击跳转 Modrinth 官网
      const iconHtml = m.icon_url
        ? `<img class="rec-icon" src="${escapeHtml(m.icon_url)}" alt="" loading="lazy" onerror="this.style.display='none'">`
        : `<span class="rec-icon rec-icon-placeholder">📦</span>`;
      li.innerHTML =
        `<div class="rec-head">${iconHtml}` +
        `<div class="rec-head-text">` +
        `<a class="rec-name" href="${escapeHtml(m.url || "https://modrinth.com/mod/" + m.slug)}" target="_blank" rel="noopener noreferrer" title="在 Modrinth 官网查看">${escapeHtml(m.title)}</a>` +
        ` <span class="pmeta">${escapeHtml(m.slug)}</span>` +
        `</div></div>` +
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
    DOM["rec-hint"].textContent = "请求失败, 请检查服务器连接!";
  }
}

// 点 👍/👎 即写库 (不经过 LLM, 不阻塞对话), 成功后标记该项并刷新统计。
// 防抖: 点击即禁用两个按钮, 请求完成才恢复 (失败) 或保持禁用 (成功),
// 避免首次请求未返回前连点发多个相同请求。
async function rateRec(slug, verdict, item, btn) {
  const btns = item.querySelectorAll(".rec-btns button");
  btns.forEach((b) => (b.disabled = true));
  try {
    const r = await API.post("/api/feedback", { slug, verdict });
    toast(r.ok ? `已记录 ${verdict === "like" ? "喜欢" : "不喜欢"} ${slug}` : (r.message || "失败"), !r.ok);
    if (!r.ok) {
      btns.forEach((b) => (b.disabled = false)); // 失败恢复可点
      return;
    }
    item.classList.add("rated");
    btn.classList.add(verdict === "like" ? "liked" : "disliked"); // 成功保持禁用 + 高亮
    // 反馈改变了口味数据, 刷新统计与标签权重
    loadProfile();
    loadInfo();
  } catch {
    btns.forEach((b) => (b.disabled = false)); // 网络失败恢复可点
    toast("请求失败, 请检查服务器连接!", true);
  }
}

/* ---------- 聊天 (NDJSON 流式) ---------- */
function setBusy(b) {
  state.busy = b;
  DOM["btn-send"].disabled = b;
  // 忙碌时显示打断按钮 (长任务/长思考时可中止当前轮)
  DOM["btn-stop"].classList.toggle("hidden", !b);
}

// 工具试用: POST /api/tool/trial, NDJSON 流式接收 tool_output + analysis_delta。
// 渲染: 用户消息 → [工具输出折叠区块] → [AI 分析 markdown 气泡 (流式)]
async function sendTrial(toolName, prompt) {
  removeWelcome();
  addMsg("user", escapeHtml(prompt));
  // 工具调用行 (与正常对话一致的视觉)
  const toolLine = addToolLine(toolName);
  // 工具输出区块 (折叠, 展示原始 JSON)
  const outputRow = document.createElement("div");
  outputRow.className = "msg-row assistant";
  const outputAvatar = document.createElement("div");
  outputAvatar.className = "avatar";
  outputAvatar.textContent = "⛏";
  const outputDiv = document.createElement("div");
  outputDiv.className = "msg assistant trial-output";
  outputDiv.innerHTML = '<div class="trial-output-head">📋 工具标准输出</div><pre class="trial-output-json">等待执行…</pre>';
  outputRow.append(outputAvatar, outputDiv);
  DOM.messages.appendChild(outputRow);
  const jsonEl = outputDiv.querySelector(".trial-output-json");
  // AI 分析气泡 (流式) + 思考提示 (工具输出后到 LLM 首 token 的空窗期提示)
  let analysisEl = null;
  let analysisText = "";
  const thinking = { el: null, timer: null };
  scrollBottom();

  chatController = new AbortController();
  try {
    const res = await fetch("/api/tool/trial", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      signal: chatController.signal,
      body: JSON.stringify({
        tool: toolName,
        prompt,
        game_version: DOM["sel-version"].value.trim() || undefined,
        loader: DOM["sel-loader"].value || undefined,
      }),
    });
    if (!res.ok || !res.body) {
      throw new Error(`HTTP ${res.status}`);
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
        if (!line) continue;
        let ev;
        try { ev = JSON.parse(line); } catch { continue; }
        if (ev.type === "tool_output") {
          // 工具执行完成: 展示原始 JSON, 工具行标记完成
          toolLine.classList.remove("pending");
          toolLine.classList.add("ok");
          toolLine.textContent = `⚙ ${ev.name} 完成`;
          jsonEl.textContent = JSON.stringify(ev.output, null, 2);
          outputDiv.querySelector(".trial-output-head").textContent = `📋 ${ev.name} 标准输出`;
          scrollBottom();
          // 工具输出后, LLM 开始分析 —— 立即显示"思考中", 告诉用户 AI 分析即将到来
          showThinking(thinking);
        } else if (ev.type === "analysis_delta") {
          // AI 分析增量: 首个 token 到达, 清除思考提示, 创建分析气泡按帧渲染
          hideThinking(thinking);
          if (!analysisEl) {
            analysisEl = addMsg("assistant", "");
          }
          analysisText += ev.text;
          queueTrialRender(analysisEl, analysisText);
        } else if (ev.type === "error") {
          hideThinking(thinking);
          addMsg("error", escapeHtml(ev.message));
        }
        // done 类型: 流结束, 无额外处理
      }
    }
  } catch (e) {
    if (e.name === "AbortError") return;
    addMsg("error", "试用请求失败: " + escapeHtml(e.message || e));
    markOffline();
  } finally {
    chatController = null;
    hideThinking(thinking);
    setBusy(false);
    refreshSidebar();
    DOM.input.focus();
  }
}

// 试用 AI 分析的流式渲染 (复用 queueMdRender 的 rAF 批处理思路, 独立队列避免与对话冲突)
let trialPaintQueued = false;
function queueTrialRender(el, text) {
  if (trialPaintQueued) return;
  trialPaintQueued = true;
  requestAnimationFrame(() => {
    trialPaintQueued = false;
    el.innerHTML = renderText(text);
    scrollBottom();
  });
}

async function send() {
  const input = DOM.input;
  const text = input.value.trim();
  if (!text || state.busy) return;
  input.value = "";
  autoGrow();
  setBusy(true);
  // 试用模式: 走独立的 /api/tool/trial 流程 (工具标准输出 + AI 分析)
  if (state.trialTool) {
    const tool = state.trialTool;
    exitTrial();
    await sendTrial(tool, text);
    return;
  }

  addMsg("user", escapeHtml(text));
  const pending = []; // { name, el, done }; replyEl 属性 = 当前流式回复气泡 (本轮共用)
  const progress = { el: null }; // 当前实时进展行 (整个 turn 共用一个, 原地更新)
  const thinking = { el: null, timer: null }; // "思考中"行状态 + 每秒计时器
  pending.replyEl = null;
  mdOverflowed = false; // 重置: 新一轮回复从实时 markdown 模式开始
  showThinking(thinking);
  chatController = new AbortController();

  try {
    const res = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      signal: chatController.signal,
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
        if (!line) continue;
        // NDJSON 流中畸形行 (网络中断半行/代理截断) 跳过, 不中断整轮流式
        let ev;
        try { ev = JSON.parse(line); } catch { continue; }
        handleEvent(ev, pending, progress, thinking);
      }
    }
  } catch (e) {
    if (e.name === "AbortError") return; // 主动切换会话丢弃旧轮输出, 不算错误
    hideThinking(thinking);
    addMsg("error", "连接失败: " + escapeHtml(e.message || e));
    markOffline(); // 发送失败即时标记离线, 不等 30s 轮询
  } finally {
    chatController = null;
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
      // LLM 等待期 (thinking 行存活时) 收到的 progress 只可能是后端 watchdog 的
      // "模型思考中… (Ns)" —— 本地"思考中"行已在计时, 忽略避免同语义重复渲染
      if (thinking.el) break;
      hideThinking(thinking);
      if (!progress.el) progress.el = addProgressLine();
      progress.el.textContent = `⏳ ${progressPrefix(ev)}${ev.text}`;
      scrollBottom();
      break;
    case "reasoning_delta": {
      // 推理型模型的实时思考片段: 只更新思考行展示, 不进对话历史
      thinking.tail = ((thinking.tail || "") + ev.text).slice(-200);
      renderThinking(thinking);
      break;
    }
    case "reply_delta": {
      // 流式打字机: 增量累积, 按帧重渲染 markdown (粗体/列表/表格流式期间即生效)。
      // scrollBottom 由 queueMdRender 的 rAF 回调统一做, 不在此重复触发。
      hideThinking(thinking);
      clearProgress(progress);
      if (!pending.replyEl) {
        pending.replyEl = addMsg("assistant", "");
        pending.replyText = "";
      }
      pending.replyText += ev.text;
      queueMdRender(pending);
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
// 对话进行中切换会话: 先打断当前轮并丢弃其流式输出 (后端在安全点收尾时会自动保存
// 已产生的内容), 再执行切换 —— 新建/导入不再被 busy 阻塞
function dropCurrentTurn() {
  API.post("/api/chat/interrupt").catch(() => {});
  chatController?.abort();
}

async function newSession() {
  if (state.busy) dropCurrentTurn();
  try {
    const r = await API.post("/api/session/new");
    toast(r.message, !r.ok);
    if (r.ok) {
      DOM.messages.innerHTML = "";
      showWelcome();
      refreshSidebar();
    }
  } catch {
    toast("请求失败, 请检查服务器连接!", true);
  }
}

// 导入指定会话文件 (点击会话列表条目触发)
async function importSession(name) {
  if (state.busy) dropCurrentTurn();
  try {
    const r = await API.post("/api/session/import", { name });
    toast(r.message, !r.ok);
    if (r.ok && Array.isArray(r.messages)) {
      renderHistory(r.messages);
      loadInfo();
    }
  } catch {
    toast("请求失败, 请检查服务器连接!", true);
  }
}

// 把导入的历史消息渲染到聊天区
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
    SET["set-model"].value = s.model;
    SET["set-base-url"].value = s.base_url;
    SET["set-api-key"].value = "";
    SET["set-key-hint"].textContent = s.api_key_set
      ? `留空保持不变 (已设置 ${s.api_key_masked})`
      : "尚未设置";
    SET["set-ctx"].value = s.context_length;
    SET["set-budget"].value = s.token_budget;
    SET["set-max-tools"].value = s.max_tool_iterations;
    SET["set-price-in"].value = s.price_input_per_m;
    SET["set-price-out"].value = s.price_output_per_m;
    SET["set-thinking"].value = s.thinking || "";
    SET["set-cf"].checked = !!s.curseforge_enabled;
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
    model: SET["set-model"].value.trim(),
    base_url: SET["set-base-url"].value.trim(),
    context_length: parseInt(SET["set-ctx"].value, 10),
    token_budget: parseInt(SET["set-budget"].value, 10),
    max_tool_iterations: parseInt(SET["set-max-tools"].value, 10),
    price_input_per_m: parseFloat(SET["set-price-in"].value),
    price_output_per_m: parseFloat(SET["set-price-out"].value),
    thinking: SET["set-thinking"].value.trim(),
    curseforge_enabled: SET["set-cf"].checked,
  };
  const key = SET["set-api-key"].value.trim();
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
    toast("请求失败, 请检查服务器连接!", true);
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

// 定时健康检查: 服务器重启/断网后顶栏即时反映, 不等用户发消息才发现失败
function startHealthPolling() {
  checkHealth();
  setInterval(checkHealth, 30000);
}

// 标记离线 (send 失败时调用, 不等下次轮询)
function markOffline() {
  DOM["status-dot"].className = "dot bad";
  DOM["status-text"].textContent = "离线";
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
// autoGrow 用 rAF 节流: input 事件高频触发, 每次设 height:auto 再读 scrollHeight
// 触发强制重排, 长文本输入卡顿。rAF 合并到一帧只重排一次。
let growQueued = false;
function autoGrow() {
  if (growQueued) return;
  growQueued = true;
  requestAnimationFrame(() => {
    growQueued = false;
    const input = $("input");
    input.style.height = "auto";
    input.style.height = Math.min(input.scrollHeight, 160) + "px";
  });
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
    toast("请求失败, 请检查服务器连接!", true);
  }
});
$("btn-new").addEventListener("click", newSession);
$("btn-theme").addEventListener("click", toggleTheme);
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
    toast("请求失败, 请检查服务器连接!", true);
  }
});
$("btn-open-dir").addEventListener("click", async () => {
  try {
    const r = await API.post("/api/packs/open");
    toast(r.message, !r.ok);
    if (r.ok) loadPacks();
  } catch {
    toast("请求失败, 请检查服务器连接!", true);
  }
});
$("btn-rec").addEventListener("click", loadRecommend);
$("btn-trial-cancel").addEventListener("click", exitTrial);
$("btn-settings").addEventListener("click", openSettings);
$("btn-settings-close").addEventListener("click", closeSettings);
$("btn-settings-cancel").addEventListener("click", closeSettings);
$("settings-mask").addEventListener("click", closeSettings);
$("settings-form").addEventListener("submit", saveSettings);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeSettings();
});

applyTheme(localStorage.getItem("rustagent-theme") || "dark");
loadPreset();
showWelcome();
startHealthPolling();
refreshSidebar();
loadTools();
loadRecommend();
DOM.input.focus();
