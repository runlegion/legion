(function () {
  "use strict";

  // -- State --

  var state = {
    agents: [],
    feed: [],
    tasks: [],
    stats: [],
    signals: [],
    health: [],
    taskFormOpen: false,
  };

  // -- Helpers --

  function relativeTime(isoStr) {
    if (!isoStr) return "";
    var diff = Math.floor((Date.now() - new Date(isoStr).getTime()) / 1000);
    if (diff < 0) return "now";
    if (diff < 60) return diff + "s";
    if (diff < 3600) return Math.floor(diff / 60) + "m";
    if (diff < 86400) return Math.floor(diff / 3600) + "h";
    return Math.floor(diff / 86400) + "d";
  }

  function truncate(text, max) {
    return text.length <= max ? text : text.slice(0, max) + "...";
  }

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function isPathLike(name) {
    return name.indexOf("/") !== -1 || name.indexOf("agent-") === 0;
  }

  function verbColor(verb) {
    var map = {
      question: "blue",
      review: "green",
      blocker: "red",
      request: "yellow",
      announce: "purple",
      answer: "cyan",
    };
    return map[verb] || "gray";
  }

  function meterColor(pct) {
    if (pct < 60) return "meter-green";
    if (pct < 85) return "meter-yellow";
    return "meter-red";
  }

  function healthColor(pct) {
    if (pct < 60) return "health-ok";
    if (pct < 85) return "health-warn";
    return "health-crit";
  }

  function postToApi(url, body) {
    return fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: body ? JSON.stringify(body) : undefined,
    });
  }

  function priorityBadgeClass(priority) {
    if (priority === "high") return "badge badge-red";
    if (priority === "med") return "badge badge-yellow";
    return "badge badge-gray";
  }

  // -- Agent strip (header bar) --

  function renderAgentStrip() {
    var strip = document.getElementById("agent-strip");
    strip.replaceChildren();

    var agents = state.agents
      .filter(function (a) { return !isPathLike(a.repo); })
      .sort(function (a, b) { return b.last_activity.localeCompare(a.last_activity); });

    agents.forEach(function (agent) {
      // Consider "active" if last activity within 10 minutes
      var diff = (Date.now() - new Date(agent.last_activity).getTime()) / 60000;
      var active = diff < 10;

      var pill = el("div", "agent-pill" + (active ? " active" : ""));
      pill.appendChild(el("span", "dot"));
      pill.appendChild(el("span", null, agent.repo));
      if (agent.unread > 0) {
        pill.appendChild(el("span", "unread", String(agent.unread)));
      }
      strip.appendChild(pill);
    });
  }

  // -- Health strip (header bar) --

  function renderHealthStrip() {
    var strip = document.getElementById("health-strip");
    strip.replaceChildren();

    if (state.health.length === 0) return;

    var host = state.health[0];
    if (!host || !host.latest) return;
    var h = host.latest;

    var items = [
      { label: "CPU", value: Math.round(h.cpu_usage_pct) + "%", pct: h.cpu_usage_pct },
      { label: "MEM", value: Math.round(h.mem_usage_pct) + "%", pct: h.mem_usage_pct },
      { label: "PRS", value: h.pressure.toFixed(1), pct: h.pressure * 100 },
      { label: "AGT", value: String(h.agents_active), pct: 0 },
    ];

    items.forEach(function (item) {
      var metric = el("span", "health-metric");
      metric.appendChild(el("span", "health-label", item.label));
      var cls = item.label === "AGT" ? "health-ok" : healthColor(item.pct);
      metric.appendChild(el("span", "health-value " + cls, item.value));
      strip.appendChild(metric);
    });
  }

  // -- Kanban board --

  function getTaskActions(status) {
    switch (status) {
      case "pending": return [{ type: "accept", label: "Accept" }];
      case "accepted": return [
        { type: "done", label: "Done" },
        { type: "block", label: "Block" },
      ];
      case "blocked": return [{ type: "unblock", label: "Unblock" }];
      default: return [];
    }
  }

  function renderKanban() {
    var board = document.getElementById("kanban-board");
    board.replaceChildren();

    var columns = [
      { key: "pending", label: "Pending" },
      { key: "accepted", label: "Active" },
      { key: "blocked", label: "Blocked" },
      { key: "done", label: "Done" },
    ];

    columns.forEach(function (col) {
      var colEl = el("div", "kanban-col col-" + col.key);
      var tasks = state.tasks.filter(function (t) { return t.status === col.key; });

      var header = el("div", "kanban-col-header");
      header.appendChild(el("span", null, col.label));
      header.appendChild(el("span", "count", String(tasks.length)));
      colEl.appendChild(header);

      var cards = el("div", "kanban-cards");

      tasks.forEach(function (task) {
        var card = el("div", "kanban-card");

        card.appendChild(el("div", "kanban-card-route", task.from_repo + " -> " + task.to_repo));
        card.appendChild(el("div", "kanban-card-text", truncate(task.text, 100)));

        var footer = el("div", "kanban-card-footer");
        footer.appendChild(el("span", priorityBadgeClass(task.priority), task.priority));
        footer.appendChild(el("span", null, relativeTime(task.created_at)));
        card.appendChild(footer);

        var actions = getTaskActions(task.status);
        if (actions.length > 0) {
          var actionsEl = el("div", "kanban-card-actions");
          actions.forEach(function (action) {
            var btn = el("button", "kanban-action-btn kanban-action-" + action.type, action.label);
            (function (taskId, actionType, button) {
              button.addEventListener("click", function () {
                button.disabled = true;
                postToApi("/api/tasks/" + taskId + "/" + actionType)
                  .then(function (r) {
                    if (r.ok) refreshTasks();
                    else button.disabled = false;
                  })
                  .catch(function () { button.disabled = false; });
              });
            })(task.id, action.type, btn);
            actionsEl.appendChild(btn);
          });
          card.appendChild(actionsEl);
        }

        cards.appendChild(card);
      });

      colEl.appendChild(cards);
      board.appendChild(colEl);
    });
  }

  // -- Task creation form --

  function renderTaskForm() {
    var container = document.getElementById("task-form-container");
    container.replaceChildren();

    if (!state.taskFormOpen) return;

    var form = el("div", "task-form");

    var row1 = el("div", "task-form-row");

    var toWrap = el("div");
    toWrap.style.flex = "1";
    toWrap.appendChild(el("label", null, "Assign to"));
    var toSelect = document.createElement("select");
    var agents = state.agents
      .filter(function (a) { return !isPathLike(a.repo); })
      .sort(function (a, b) { return a.repo.localeCompare(b.repo); });
    agents.forEach(function (a) {
      var opt = document.createElement("option");
      opt.value = a.repo;
      opt.textContent = a.repo;
      toSelect.appendChild(opt);
    });
    toWrap.appendChild(toSelect);
    row1.appendChild(toWrap);

    var prioWrap = el("div");
    prioWrap.style.flex = "1";
    prioWrap.appendChild(el("label", null, "Priority"));
    var prioSelect = document.createElement("select");
    ["low", "med", "high"].forEach(function (p) {
      var opt = document.createElement("option");
      opt.value = p;
      opt.textContent = p;
      if (p === "med") opt.selected = true;
      prioSelect.appendChild(opt);
    });
    prioWrap.appendChild(prioSelect);
    row1.appendChild(prioWrap);

    form.appendChild(row1);

    form.appendChild(el("label", null, "Description"));
    var textArea = document.createElement("textarea");
    textArea.rows = 2;
    textArea.placeholder = "What needs to be done?";
    form.appendChild(textArea);

    var errorEl = el("div", "task-form-error");
    form.appendChild(errorEl);

    var submitRow = el("div", "task-form-row");
    var submitBtn = el("button", "broadcast-btn", "Create");
    submitBtn.addEventListener("click", function () {
      var text = textArea.value.trim();
      if (!text) { errorEl.textContent = "Description required"; return; }
      submitBtn.disabled = true;
      postToApi("/api/tasks/create", {
        from: "meatbag",
        to: toSelect.value,
        text: text,
        priority: prioSelect.value,
      })
        .then(function (r) {
          if (r.ok) {
            state.taskFormOpen = false;
            renderTaskForm();
            refreshTasks();
          } else {
            return r.json().then(function (d) { errorEl.textContent = d.error; });
          }
        })
        .catch(function (e) { errorEl.textContent = String(e); })
        .finally(function () { submitBtn.disabled = false; });
    });
    submitRow.appendChild(submitBtn);

    var cancelBtn = el("button", "panel-action-btn", "Cancel");
    cancelBtn.addEventListener("click", function () {
      state.taskFormOpen = false;
      renderTaskForm();
    });
    submitRow.appendChild(cancelBtn);
    form.appendChild(submitRow);

    container.appendChild(form);
  }

  // -- Agent detail sidebar --

  function renderAgentDetail() {
    var container = document.getElementById("agent-detail");
    container.replaceChildren();

    // Machine performance card first
    if (state.health.length > 0 && state.health[0].latest) {
      container.appendChild(buildPerfCard(state.health[0]));
    }

    var agents = state.agents
      .filter(function (a) { return !isPathLike(a.repo); })
      .sort(function (a, b) { return b.last_activity.localeCompare(a.last_activity); });

    agents.forEach(function (agent) {
      var card = el("div", "agent-card");

      var header = el("div", "agent-card-header");
      var diff = (Date.now() - new Date(agent.last_activity).getTime()) / 60000;
      var statusBadge = diff < 10 ? "badge badge-green" : "badge badge-gray";
      var statusText = diff < 10 ? "active" : "idle";
      header.appendChild(el("span", "agent-card-name", agent.repo));
      header.appendChild(el("span", statusBadge, statusText));
      card.appendChild(header);

      var stats = el("div", "agent-card-stats");

      var pairs = [
        ["reflections", String(agent.reflection_count)],
        ["boosts", String(agent.boost_sum)],
        ["posts", String(agent.team_post_count)],
        ["unread", String(agent.unread)],
      ];
      pairs.forEach(function (p) {
        var row = el("div", "agent-stat");
        row.appendChild(el("span", "agent-stat-label", p[0]));
        row.appendChild(el("span", "agent-stat-value", p[1]));
        stats.appendChild(row);
      });
      card.appendChild(stats);

      // Task dots for this agent
      var agentTasks = state.tasks.filter(function (t) {
        return t.to_repo === agent.repo && t.status !== "done";
      });
      if (agentTasks.length > 0) {
        var dotsRow = el("div", "agent-tasks");
        agentTasks.forEach(function (t) {
          dotsRow.appendChild(el("span", "agent-task-dot task-dot-" + t.status));
        });
        card.appendChild(dotsRow);
      }

      card.appendChild(el("div", "agent-card-time", relativeTime(agent.last_activity) + " ago"));

      container.appendChild(card);
    });
  }

  function buildPerfCard(hostData) {
    var card = el("div", "perf-card");
    card.appendChild(el("h3", null, "Machine: " + hostData.hostname));

    var h = hostData.latest;

    var rows = [
      { label: "CPU", value: Math.round(h.cpu_usage_pct) + "%", pct: h.cpu_usage_pct },
      { label: "Memory", value: Math.round(h.mem_usage_pct) + "%", pct: h.mem_usage_pct },
      { label: "Pressure", value: (h.pressure * 1).toFixed(2), pct: h.pressure * 100 },
    ];

    if (h.load_avg_1 !== null) {
      rows.push({
        label: "Load",
        value: h.load_avg_1.toFixed(1) + " / " + (h.load_avg_5 || 0).toFixed(1) + " / " + (h.load_avg_15 || 0).toFixed(1),
        pct: (h.load_avg_1 / h.cpu_core_count) * 100,
      });
    }

    rows.push({ label: "Agents", value: String(h.agents_active), pct: 0 });

    if (h.cpu_temp_celsius !== null && h.cpu_temp_celsius !== undefined) {
      rows.push({
        label: "Temp",
        value: Math.round(h.cpu_temp_celsius) + "C",
        pct: h.cpu_temp_celsius > 80 ? 90 : h.cpu_temp_celsius > 60 ? 70 : 40,
      });
    }

    rows.forEach(function (row) {
      var perfRow = el("div", "perf-row");
      perfRow.appendChild(el("span", "perf-label", row.label));
      var valEl = el("span", "perf-value " + healthColor(row.pct), row.value);
      perfRow.appendChild(valEl);
      card.appendChild(perfRow);

      if (row.label !== "Agents" && row.label !== "Load") {
        var meter = el("div", "meter");
        var fill = el("div", "meter-fill " + meterColor(row.pct));
        fill.style.width = Math.min(row.pct, 100) + "%";
        meter.appendChild(fill);
        card.appendChild(meter);
      }
    });

    return card;
  }

  // -- Signals feed --

  function renderSignals() {
    var feed = document.getElementById("signals-feed");
    feed.replaceChildren();

    if (state.signals.length === 0) {
      feed.appendChild(el("div", "bullpen-item", "No signals yet"));
      return;
    }

    state.signals.slice(0, 30).forEach(function (sig) {
      var color = verbColor(sig.verb);
      var item = el("div", "signal-item signal-border-" + color);

      var header = el("div", "signal-item-header");
      header.appendChild(el("span", "badge badge-green", sig.from_repo));
      header.appendChild(el("span", "signal-arrow", "->"));
      header.appendChild(el("span", "badge badge-" + color, sig.verb));
      if (sig.status) {
        header.appendChild(el("span", "badge badge-gray", sig.status));
      }
      header.appendChild(el("span", "badge badge-blue", "@" + sig.to));
      header.appendChild(el("span", "signal-time", relativeTime(sig.created_at)));
      item.appendChild(header);

      var text = el("div", "signal-text signal-text-truncated", sig.text);
      item.appendChild(text);

      // Click to expand
      (function (textEl, fullText) {
        textEl.addEventListener("click", function () {
          if (textEl.classList.contains("signal-text-truncated")) {
            textEl.classList.remove("signal-text-truncated");
          } else {
            textEl.classList.add("signal-text-truncated");
          }
        });
        textEl.style.cursor = "pointer";
      })(text, sig.text);

      feed.appendChild(item);
    });
  }

  // -- Bullpen feed --

  function renderBullpen() {
    var bar = document.getElementById("broadcast-bar");
    bar.replaceChildren();

    var row = el("div", "broadcast-row");
    var textarea = document.createElement("textarea");
    textarea.className = "broadcast-input";
    textarea.rows = 1;
    textarea.placeholder = "@agent message... or post to bullpen";
    row.appendChild(textarea);

    var sendBtn = el("button", "broadcast-btn", "Send");
    sendBtn.addEventListener("click", function () {
      var text = textarea.value.trim();
      if (!text) return;
      sendBtn.disabled = true;
      postToApi("/api/post", { repo: "meatbag", text: text })
        .then(function (r) { if (r.ok) textarea.value = ""; })
        .catch(function () {})
        .finally(function () { sendBtn.disabled = false; });
    });
    textarea.addEventListener("keydown", function (e) {
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
        e.preventDefault();
        sendBtn.click();
      }
    });
    row.appendChild(sendBtn);
    bar.appendChild(row);

    // Feed items (musings only -- signals have their own panel)
    var feed = document.getElementById("bullpen-feed");
    feed.replaceChildren();

    var musings = state.feed.filter(function (item) { return !item.is_signal; });

    if (musings.length === 0) {
      feed.appendChild(el("div", "bullpen-item", "No posts yet"));
      return;
    }

    musings.slice(0, 30).forEach(function (item) {
      var card = el("div", "bullpen-item");

      var header = el("div", "bullpen-item-header");
      header.appendChild(el("span", "badge badge-green", item.repo));
      header.appendChild(el("span", "bullpen-time", relativeTime(item.created_at)));

      var boostBtn = el("button", "boost-btn", "+boost");
      (function (id, btn) {
        btn.addEventListener("click", function () {
          btn.disabled = true;
          postToApi("/api/boost/" + id)
            .then(function (r) {
              if (r.ok) { btn.textContent = "boosted"; btn.classList.add("boosted"); }
              else btn.disabled = false;
            })
            .catch(function () { btn.disabled = false; });
        });
      })(item.id, boostBtn);
      header.appendChild(boostBtn);

      card.appendChild(header);

      var textEl = el("div", "bullpen-text");
      var full = item.text;
      var short = truncate(full, 180);
      textEl.textContent = short;

      if (full.length > 180) {
        textEl.classList.add("bullpen-text-truncated");
        var toggle = el("button", "show-more-btn", "more");
        var expanded = false;
        toggle.addEventListener("click", function () {
          expanded = !expanded;
          textEl.textContent = expanded ? full : short;
          textEl.classList.toggle("bullpen-text-truncated", !expanded);
          toggle.textContent = expanded ? "less" : "more";
        });
        card.appendChild(textEl);
        card.appendChild(toggle);
      } else {
        card.appendChild(textEl);
      }

      feed.appendChild(card);
    });
  }

  // -- Live boosts cloud --

  function renderBoostCloud() {
    var cloud = document.getElementById("boost-cloud");
    cloud.replaceChildren();

    // Build boost data from agents
    var boosts = state.agents
      .filter(function (a) { return !isPathLike(a.repo) && a.boost_sum > 0; })
      .sort(function (a, b) { return b.boost_sum - a.boost_sum; });

    if (boosts.length === 0) {
      cloud.appendChild(el("span", "boost-tag", "No boosts yet"));
      return;
    }

    var maxBoost = boosts[0].boost_sum;

    boosts.forEach(function (agent) {
      var hot = agent.boost_sum > maxBoost * 0.5;
      var tag = el("span", "boost-tag" + (hot ? " boost-tag-hot" : ""));
      tag.appendChild(el("span", null, agent.repo));
      tag.appendChild(el("span", "boost-count", String(agent.boost_sum)));
      cloud.appendChild(tag);
    });
  }

  // -- Full render --

  function renderAll() {
    renderAgentStrip();
    renderHealthStrip();
    renderKanban();
    renderTaskForm();
    renderAgentDetail();
    renderSignals();
    renderBullpen();
    renderBoostCloud();
  }

  // -- Data fetching --

  function fetchJSON(url, key, callback) {
    fetch(url)
      .then(function (r) { return r.json(); })
      .then(function (data) {
        state[key] = data;
        if (callback) callback();
      })
      .catch(function (err) {
        console.error("[legion] fetch " + url + " failed:", err);
      });
  }

  function refreshTasks() {
    fetchJSON("/api/tasks", "tasks", function () {
      renderKanban();
      renderAgentDetail();
    });
  }

  // Initial fetches
  var loaded = 0;
  var totalFetches = 6;

  function onLoaded() {
    loaded++;
    if (loaded >= totalFetches) renderAll();
  }

  fetchJSON("/api/agents", "agents", onLoaded);
  fetchJSON("/api/feed", "feed", onLoaded);
  fetchJSON("/api/tasks", "tasks", onLoaded);
  fetchJSON("/api/stats", "stats", onLoaded);
  fetchJSON("/api/signals", "signals", onLoaded);
  fetchJSON("/api/health", "health", onLoaded);

  // Task create button
  document.getElementById("task-create-btn").addEventListener("click", function () {
    state.taskFormOpen = !state.taskFormOpen;
    renderTaskForm();
  });

  // -- SSE real-time updates --

  var retryDelay = 1000;

  function connectSSE() {
    var source = new EventSource("/sse");

    source.onopen = function () {
      retryDelay = 1000;
    };

    source.addEventListener("agents", function (event) {
      try {
        state.agents = JSON.parse(event.data);
        renderAgentStrip();
        renderAgentDetail();
        renderBoostCloud();
      } catch (e) {}
    });

    source.addEventListener("feed", function (event) {
      try {
        state.feed = JSON.parse(event.data);
        renderBullpen();
        // Re-fetch signals since they come from feed
        fetchJSON("/api/signals", "signals", renderSignals);
      } catch (e) {}
    });

    source.addEventListener("tasks", function (event) {
      try {
        state.tasks = JSON.parse(event.data);
        renderKanban();
        renderAgentDetail();
      } catch (e) {}
    });

    source.addEventListener("ping", function () {});

    source.onerror = function () {
      source.close();
      setTimeout(connectSSE, retryDelay);
      retryDelay = Math.min(retryDelay * 2, 30000);
    };
  }

  connectSSE();

  // Periodic health refresh (every 30s)
  setInterval(function () {
    fetchJSON("/api/health", "health", function () {
      renderHealthStrip();
      renderAgentDetail();
    });
  }, 30000);
})();
