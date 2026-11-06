/* Berthio 面板前端（M2）
 * 单一来源：与设计稿同一套 DOM / 组件；本文件只负责数据接入与交互。
 * 静态 demo 模式通过 window.__BERTHIO_DEMO_DATA__ 注入同一接口形状的假数据，
 * 不另写一套 UI。 */
(function () {
  'use strict';

  var DEMO = !!window.__BERTHIO_DEMO_DATA__;
  var FIX = window.__BERTHIO_DEMO_DATA__ || null;
  var AUTH = { csrf: '', demo: DEMO, status: null };

  function $(s, r) { return (r || document).querySelector(s); }
  function $$(s, r) { return Array.prototype.slice.call((r || document).querySelectorAll(s)); }
  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function clone(v) { return JSON.parse(JSON.stringify(v)); }
  function fmtBytes(n) {
    n = Number(n) || 0;
    var u = ['B', 'KB', 'MB', 'GB', 'TB'], i = 0, v = n;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return i === 0 ? n + ' B' : v.toFixed(1) + ' ' + u[i];
  }
  function dash(s) { return s ? s : '—'; }

  var PAGE_META = {
    overview: ['概览', '服务状态、入站与内核探测'],
    inbound: ['入站', '入站列表、客户端、用量与配额、新建向导'],
    export: ['配置导出', '节点多选 + 规则组 + 订阅令牌 + 完整文件'],
    kernels: ['内核', '能力探测与版本矩阵；未执行自检一律标未验证'],
    logs: ['日志', '审计事件流（只读、已脱敏）'],
    settings: ['设置', '对外地址、数据目录与访问边界']
  };

  var UI = {
    panel: null, inbounds: [], rules: null, kernels: null, logs: null, defaults: null,
    sources: null, lastFetch: null,
    update: { status: null, poll: null, lastAction: 'check' },
    export: {
      format: 'clash', scenario: 'desktop', bucket: '112',
      rules: {}, order: [], preset: 'daily', nodeSel: {}, preview: null, ack: false, timer: null
    }
  };

  function defaultDemoUpdate() {
    return {
      current_version: (FIX && FIX.panel && FIX.panel.version) || '1.0.0',
      homepage: 'https://github.com/yunyu697/berthio',
      feed: 'https://github.com/yunyu697/berthio/releases/download/updates/latest.json',
      mirror: 'auto', mirrors: [], has_pubkey: true, install_supported: true, demo: true,
      phase: 'idle', busy: false, checked: false, available: null,
      progress: null, attempts: [], error: null, saved: null, last_install: null
    };
  }

  // 演示模式下的静态导入预览（与真实接口同一形状，不写状态）。
  var DEMO_IMPORT_PREVIEW = {
    source_kind: 'json',
    schema: { shape: 'inbounds 数组', notes: ['演示数据'] },
    source: { label: 'demo.json', bytes: 1024, sha256: 'demo' },
    counts: { import: 2, skip: 1, conflict: 1, already_imported: 0, source_rows: 4 },
    preview_id: 'demo-preview',
    plan_hash: 'demo-plan-hash',
    items: [
      { position: 1, kind: 'import', source_id: 1, source_ref: 'demo-1', remark: '演示 · Reality', mapped: { protocol: 'vless', network: 'tcp', port: 2443, l4: 'tcp', enabled: true }, reasons: [] },
      { position: 2, kind: 'import', source_id: 2, source_ref: 'demo-2', remark: '演示 · TLS+WS', mapped: { protocol: 'vless', network: 'ws', port: 28443, l4: 'tcp', enabled: true }, reasons: [] },
      { position: 3, kind: 'skip', source_id: 3, source_ref: '', remark: '演示 · gRPC', mapped: { protocol: 'vless', network: 'grpc', port: 27447, l4: 'tcp', enabled: true }, reasons: ['传输方式 grpc 暂无安全映射，已跳过'] },
      { position: 4, kind: 'conflict', source_id: 4, source_ref: '', remark: '演示 · 坏 Reality', mapped: { protocol: 'vless', network: 'tcp', port: 27443, l4: 'tcp', enabled: true }, reasons: ['realitySettings 缺少 privateKey（只有 publicKey），xray -test 会拒绝'] }
    ]
  };

  /* ---------------- 数据层：真实接口 / 静态 demo 同一形状 ---------------- */
  function demoPost(path, body) {
    if (path === '/api/inbound/inbounds') {
      var id = 'inb_demo' + String(Math.random()).slice(2, 8);
      var item = {
        id: id, tag: 'vless-tcp-443-demo', remark: body.remark || '演示新建', protocol: 'vless',
        network: body.network || 'tcp', security: body.security || 'reality',
        listen: body.listen || '0.0.0.0', port: body.port || 443, l4: 'tcp',
        server_kernel: 'xray', server_version: '', enabled: true,
        clients: 1, enabled_clients: 1, created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(), source: 'manual', status: '待核验',
        verification: '未验证', today_bytes: 0
      };
      UI.inbounds.push(item);
      return { committed: true, item: item, warnings: ['演示数据：未写入真实状态'], transaction: { steps: [] } };
    }
    if (path === '/api/export/tokens') {
      return {
        id: 'tok_demo', token: 'bto_demo_' + String(Math.random()).slice(2, 10),
        url: '/sub/bto_demo?target=' + (body.target || 'clash'), target: body.target || 'clash',
        note: '演示数据：token 不会写入真实状态'
      };
    }
    if (path === '/api/panel') {
      if (FIX && FIX.panel) FIX.panel.panel_address = body.panel_address || FIX.panel.panel_address;
      return { ok: true, panel_address: body.panel_address };
    }
    if (path === '/api/update/check') {
      var u = FIX.update || (FIX.update = defaultDemoUpdate());
      u.phase = 'checking'; u.busy = true; u.error = null;
      setTimeout(function () {
        u.phase = 'latest'; u.busy = false; u.checked = true; u.available = false;
        u.version = u.current_version;
        u.attempts = [{ source: '官方 GitHub', url: '', ok: true, error: '' }];
      }, 700);
      return { started: true };
    }
    return { ok: true };
  }

  /* 静态 demo 里的镜像操作：只改内存里的快照数据，展示真实前端交互 */
  function demoMirror(method, key, body) {
    if (!FIX.sources) return { ok: true };
    if (key === '/api/rulesets/fetch') {
      var fk = (body.id || '') + '|' + (body.format || 'mihomo') + '|' + (body.source || 'auto');
      var hit = (FIX.fetchResults || {})[fk];
      if (hit) return clone(hit);
      return clone({
        ok: false, id: body.id, format: body.format || 'mihomo', attempts: [],
        baseline: null, chosen: null, total_ms: 0,
        note: '演示数据：该组合未预置拉取结果，真实模式下会实际下载并校验'
      });
    }
    if (key === '/api/rulesets/sources/test') {
      var t = (FIX.sourceTests || {})[body.id];
      return clone(t || {
        ok: true, source: body.id, ms: 118, bytes: 532, matches_baseline: true,
        detail: '演示数据：预置测速结论'
      });
    }
    if (method === 'PUT' && key === '/api/rulesets/sources/mode') {
      FIX.sources.mode = body.mode;
      FIX.sources.selected = body.mode;
      return { ok: true, mode: body.mode };
    }
    if (method === 'POST' && key === '/api/rulesets/sources') {
      var id = 'custom-demo' + String(Math.random()).slice(2, 6);
      FIX.sources.sources.push({
        id: id, label: body.label, kind: 'third', locked: false, enabled: true,
        priority: Number(body.priority) || 6, url_mihomo: body.url_mihomo,
        url_singbox: body.url_singbox || body.url_mihomo, note: '演示数据', last: null
      });
      return { ok: true, id: id };
    }
    if (method === 'PATCH' && key.indexOf('/api/rulesets/sources/') === 0) {
      var pid = decodeURIComponent(key.slice('/api/rulesets/sources/'.length));
      var ps = FIX.sources.sources.filter(function (x) { return x.id === pid; })[0];
      if (ps) {
        if (body.enabled !== undefined) ps.enabled = !!body.enabled;
        if (body.priority !== undefined) ps.priority = Number(body.priority) || ps.priority;
        if (body.url_mihomo) ps.url_mihomo = body.url_mihomo;
        if (body.url_singbox) ps.url_singbox = body.url_singbox;
      }
      return { ok: true, id: pid };
    }
    if (method === 'DELETE' && key.indexOf('/api/rulesets/sources/') === 0) {
      var did = decodeURIComponent(key.slice('/api/rulesets/sources/'.length));
      FIX.sources.sources = FIX.sources.sources.filter(function (x) { return x.id !== did; });
      return { ok: true, id: did };
    }
    return { ok: true };
  }

  function api(path, opts) {
    if (DEMO) {
      var key0 = path.split('?')[0];
      var method0 = (opts && opts.method) || 'GET';
      if (method0 !== 'GET') {
        var bod = JSON.parse((opts && opts.body) || '{}');
        if (key0 === '/api/migrate/preview') return Promise.resolve(clone(DEMO_IMPORT_PREVIEW));
        if (key0 === '/api/migrate/apply') {
          return Promise.resolve({
            demo: true, imported: 0, items: clone(DEMO_IMPORT_PREVIEW.items),
            counts: clone(DEMO_IMPORT_PREVIEW.counts),
            message: '演示模式：不会写入真实状态'
          });
        }
        if (key0.indexOf('/api/rulesets/') === 0) return Promise.resolve(demoMirror(method0, key0, bod));
        return Promise.resolve(demoPost(key0, bod));
      }
      if (path.indexOf('/api/export/preview') === 0) {
        return Promise.resolve(clone(FIX.exportPreview));
      }
      var key = path.split('?')[0];
      if (key === '/api/logs') return Promise.resolve(clone(FIX.logs));
      if (key === '/api/rulesets') return Promise.resolve(clone(FIX.rulesets));
      if (key === '/api/rulesets/sources') return Promise.resolve(clone(FIX.sources || { sources: [] }));
      if (key === '/api/kernels') return Promise.resolve(clone(FIX.kernels));
      if (key === '/api/auth/status') return Promise.resolve({ initialized: true, authenticated: true, demo: true, csrf_token: 'demo-csrf' });
      if (key === '/api/inbound/inbounds') return Promise.resolve({ items: clone(UI.inbounds), total: UI.inbounds.length, kernels: { xray: UI.inbounds.length }, selection: {} });
      if (key === '/api/inbound/wizard/defaults') return Promise.resolve(clone(FIX.defaults));
      if (key === '/api/panel') return Promise.resolve(clone(FIX.panel));
      if (key === '/api/update/status') return Promise.resolve(clone(FIX.update || defaultDemoUpdate()));
      if (key.indexOf('/api/inbound/inbounds/') === 0 && key.slice(-8) === '/clients') {
        var iid = decodeURIComponent(key.slice('/api/inbound/inbounds/'.length, key.length - '/clients'.length));
        return Promise.resolve({ inbound_id: iid, items: clone((FIX.clients || {})[iid] || []) });
      }
      if (key.indexOf('/api/kernels') === 0) {
        return Promise.resolve({ ok: false, demo: true, message: '演示模式不启动真实内核' });
      }
      return Promise.resolve({});
    }
    var method = (opts && opts.method) || 'GET';
    var headers = { 'Content-Type': 'application/json' };
    if (method !== 'GET' && AUTH.csrf) headers['X-Berthio-CSRF'] = AUTH.csrf;
    var init = Object.assign({ credentials: 'same-origin' }, opts || {});
    init.headers = headers;
    return fetch(path, init)
      .then(function (res) {
        return res.text().then(function (text) {
          var body = text;
          try { body = JSON.parse(text); } catch (e) {}
          if (!res.ok) {
            if (res.status === 401 && !DEMO) showAuth('login');
            var msg = (body && body.error && body.error.message) || ('HTTP ' + res.status);
            var err = new Error(msg);
            err.status = res.status;
            err.code = body && body.error && body.error.code;
            err.fields = (body && body.error && body.error.fields) || [];
            throw err;
          }
          return body;
        });
      });
  }

  /* ---------------- 通用 UI ---------------- */
  var toastTimer = null;
  function toast(title, sub) {
    var el = $('#toast');
    $('#toast-title').textContent = title;
    $('#toast-sub').textContent = sub || '';
    el.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { el.classList.remove('show'); }, 3200);
  }

  var dark = false;
  try { dark = localStorage.getItem('berthio-theme') === 'dark'; } catch (e) {}
  function applyTheme(d) {
    document.body.classList.toggle('dark', d);
    var s = $('.ico-sun'), m = $('.ico-moon');
    if (s) s.style.display = d ? 'none' : 'block';
    if (m) m.style.display = d ? 'block' : 'none';
    try { localStorage.setItem('berthio-theme', d ? 'dark' : 'light'); } catch (e) {}
  }
  applyTheme(dark);

  function showPage(id) {
    if (!PAGE_META[id]) return;
    $$('.page').forEach(function (p) { p.classList.toggle('active', p.id === 'page-' + id); });
    $$('.nav-item').forEach(function (n) {
      var on = n.dataset.page === id;
      n.classList.toggle('active', on);
      if (on) n.setAttribute('aria-current', 'page'); else n.removeAttribute('aria-current');
    });
    $('#page-title').textContent = PAGE_META[id][0];
    $('#page-sub').textContent = PAGE_META[id][1];
    window.scrollTo(0, 0);
    if (id === 'export') schedulePreview();
  }

  function badgeClass(status) {
    switch (status) {
      case 'PASS': return 'ok';
      case 'FAIL': return 'warn';
      case 'WARN': return 'warn';
      default: return 'muted';
    }
  }

  /* ---------------- 启动 ---------------- */
  function boot() {
    $$('.nav-item').forEach(function (n) {
      n.addEventListener('click', function () { showPage(n.dataset.page); });
    });
    $$('[data-page-jump]').forEach(function (b) {
      b.addEventListener('click', function () { showPage(b.dataset.pageJump); });
    });
    var themeBtn = $('#theme-toggle');
    if (themeBtn) themeBtn.addEventListener('click', function () { applyTheme(!document.body.classList.contains('dark')); });
    $('#wizard-start').addEventListener('click', openWizard);
    $('#wizard-close').addEventListener('click', closeWizard);
    $('#wizard-cancel').addEventListener('click', closeWizard);
    $('#wizard-back').addEventListener('click', function () { wizardGo(-1); });
    $('#wizard-commit').addEventListener('click', function () { if (wizardStep >= 6) commitWizard(); else wizardGo(1); });
    $('#panel-save').addEventListener('click', savePanelConfig);
    $('#tls-generate').addEventListener('click', generateTlsCert);
    $('#wz-protocol').addEventListener('change', updateWizardFields);
    $('#wz-port').addEventListener('input', updateWizardFields);

    bindAuthEvents();
    bindProcessEvents();
    bindExportEvents();
    bindTokenActions();
    bindMirrorEvents();
    bindImportEvents();
    bindUpdateEvents();
    if (DEMO) {
      AUTH.csrf = 'demo-csrf';
      loadAll();
    } else {
      authBoot();
    }
  }

  function loadAll() {
    return Promise.all([
      api('/api/panel'), api('/api/inbound/inbounds'), api('/api/rulesets'), api('/api/kernels'), api('/api/logs'),
      api('/api/inbound/wizard/defaults'), api('/api/rulesets/sources'), api('/api/update/status')
    ]).then(function (r) {
      UI.panel = r[0];
      UI.inbounds = r[1].items || [];
      UI.rules = r[2];
      UI.kernels = r[3];
      UI.logs = r[4];
      UI.defaults = r[5];
      UI.sources = r[6];
      UI.update.status = r[7];
      if ((UI.panel && UI.panel.demo) || DEMO) $('#demo-banner').classList.add('show');
      renderChrome();
      renderOverview();
      renderInbounds();
      renderRulesets();
      renderNodes();
      renderKernels();
      renderProcesses();
      renderMirrorCard();
      renderLogs();
      renderSettings();
      renderUpdate();
      initExportDefaults();
      schedulePreview();
    }).catch(function (e) {
      toast('接口请求失败', e.message);
    });
  }

  /* ---------------- 顶栏 / 状态 ---------------- */
  function renderChrome() {
    var p = UI.panel || {};
    var counts = p.counts || {};
    $('#nav-count-inbound').textContent = counts.inbounds || UI.inbounds.length;
    var present = (UI.kernels && UI.kernels.binaries || []).filter(function (b) { return b.present; }).length;
    $('#nav-count-kernels').textContent = present;
    $('#nav-count-logs').textContent = (UI.logs && UI.logs.items || []).length;
    $('#side-status-text').textContent = (p.version || '?') + ' · 内核已探测 ' + present + '/3';
    $('#about-version').textContent = p.version || '—';
    $('#panel-version-input').value = p.version || '—';
    var hk = $('#hero-kernel');
    if (hk) hk.textContent = present ? (present + '/3 已探测 · 未接管') : '未安装 · 仅探测';
    var alert = $('.alert-strip');
    if (alert && UI.kernels) {
      var missing = (UI.kernels.binaries || []).filter(function (b) { return !b.present; }).map(function (b) { return b.name; });
      alert.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l9 16H3z"></path><path d="M12 9v4M12 16.5v.1"></path></svg>' +
        '<div><b>' + (missing.length ? '未找到 ' + missing.join(' / ') : '内核已探测') + '</b> <span>· 缺失二进制的能力一律标「未验证」，不会写成「已验证」。</span></div>' +
        '<button class="btn btn-sm" data-page-jump="kernels">查看探测</button>';
      $$('[data-page-jump="kernels"]', alert).forEach(function (b) {
        b.addEventListener('click', function () { showPage('kernels'); });
      });
    }
    var body = $('#onboard-body');
    if (body) {
      body.innerHTML = '<div class="onboard-head"><div class="onboard-line"><b>M2 已接入规则集镜像换源</b><span>入站列表 / 新建向导 / 导出 / 内核探测 / 官方 SHA-256 校验</span></div><span class="badge info plain">' + esc(p.version || '') + '</span></div>' +
        '<div class="onboard-steps">' +
        '<div class="onboard-step"><i>1</i><div><b>新建第一个入站</b><span>Reality 密钥由后端生成，私钥只落盘</span></div></div>' +
        '<div class="onboard-step"><i>2</i><div><b>导出可校验的配置</b><span>L0/L1/L2，缺失二进制标未验证</span></div></div>' +
        '<div class="onboard-step"><i>3</i><div><b>确认本机能力</b><span>版本探测 + 能力矩阵，不默认「已验证」</span></div></div>' +
        '</div>';
    }
  }

  function renderOverview() {
    var box = $('#overview-stats');
    if (!box) return;
    var inb = UI.inbounds;
    var clients = inb.reduce(function (a, i) { return a + (i.clients || 0); }, 0);
    var enabledIn = inb.filter(function (i) { return i.enabled; }).length;
    var present = (UI.kernels && UI.kernels.binaries || []).filter(function (b) { return b.present; });
    var total = (UI.rules && UI.rules.catalog || []).length;
    box.innerHTML =
      statCard('入站数量', inb.length + ' <small>个</small>', '启用 ' + enabledIn + ' · 停用 ' + (inb.length - enabledIn), 'info') +
      statCard('客户端', clients + ' <small>个</small>', '全部用于导出与订阅', 'ok') +
      statCard('已探测内核', present.length + '/3', present.length ? present.map(function (b) { return b.name + ' ' + (b.version || '?'); }).join(' · ') : '未找到任何二进制', 'muted') +
      statCard('规则集', total + ' <small>条</small>', '官方基准 · 已缓存 ' + (((UI.rules || {}).cache || {}).count || 0) + ' 条 · 镜像可选', 'accent');
  }

  function statCard(label, value, foot, kind) {
    return '<div class="stat"><div class="stat-top"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"></circle><path d="M12 3v3M12 18v3M3 12h3M18 12h3"></path></svg>' +
      label + '<span class="badge ' + kind + ' plain">M2</span></div><div class="stat-value">' + value + '</div>' +
      '<div class="stat-foot"><span>' + esc(foot) + '</span></div></div>';
  }

  /* ---------------- 入站列表 ---------------- */
  function renderInbounds() {
    var tbody = $('#inbound-tbody');
    if (!tbody) return;
    if (!UI.inbounds.length) {
      tbody.innerHTML = '<tr><td colspan="8"><div class="table-note">还没有入站。点击右上角「打开向导」创建第一个。</div></td></tr>';
    } else {
      tbody.innerHTML = UI.inbounds.map(function (i) {
        var stateBadge = i.enabled ? '<span class="badge warn plain">未验证</span>' : '<span class="badge muted plain">已停用</span>';
        return '<tr data-inbound="' + esc(i.id) + '">' +
          '<td><div class="cell-main"><b>' + esc(i.remark) + '</b><span class="badge muted">' + esc(i.tag) + '</span></div></td>' +
          '<td class="mono">' + esc(i.protocol) + ' + ' + esc(i.security) + '</td>' +
          '<td class="mono">' + esc(i.port) + '/' + esc(i.l4) + '</td>' +
          '<td><span class="badge ' + (i.server_kernel === 'xray' ? 'info' : 'accent') + ' plain">' + esc(i.server_kernel) + '</span></td>' +
          '<td>' + esc(i.clients) + '</td><td class="mono">' + fmtBytes(i.today_bytes) + '</td>' +
          '<td>' + stateBadge + '</td>' +
          '<td style="text-align:right"><button class="btn btn-sm js-row-export" data-inbound="' + esc(i.id) + '">导出</button></td></tr>';
      }).join('');
    }
    $('#inbound-count-badge').textContent = UI.inbounds.length + ' 个入站';
    var k = {};
    UI.inbounds.forEach(function (i) { k[i.server_kernel] = (k[i.server_kernel] || 0) + 1; });
    $('#inbound-kernel-badge').textContent = Object.keys(k).map(function (x) { return x + ' ' + k[x]; }).join(' · ') || '内核 —';
    $$('.js-row-export', tbody).forEach(function (b) {
      b.addEventListener('click', function () {
        var inbound = UI.inbounds.filter(function (i) { return i.id === b.dataset.inbound; })[0];
        if (!inbound) return;
        prefetchClients(inbound).then(function (clients) {
          UI.export.nodeSel = {};
          clients.forEach(function (c) { if (c.enabled && inbound.enabled) UI.export.nodeSel[c.id] = true; });
          UI.export.inboundSel = {}; UI.export.inboundSel[inbound.id] = true;
          showPage('export');
        });
      });
    });
  }

  /* ---------------- 从 3x-ui 导入（JSON 主路径 / DB 兜底） ---------------- */
  var IMPORT = { mode: 'paste', fileText: '', fileLabel: '', plan: null };

  function bindImportEvents() {
    var tabs = $$('#import-source-tabs button');
    tabs.forEach(function (b) {
      b.addEventListener('click', function () {
        tabs.forEach(function (x) { x.classList.toggle('active', x === b); });
        IMPORT.mode = b.dataset.importMode;
        $('#import-paste-pane').style.display = IMPORT.mode === 'paste' ? '' : 'none';
        $('#import-file-pane').style.display = IMPORT.mode === 'file' ? '' : 'none';
        $('#import-db-pane').style.display = IMPORT.mode === 'db' ? '' : 'none';
        resetImport();
      });
    });
    $('#import-json').addEventListener('input', resetImport);
    $('#import-file').addEventListener('change', function () {
      var f = this.files && this.files[0];
      IMPORT.fileText = '';
      IMPORT.fileLabel = '';
      resetImport();
      if (!f) return;
      if (f.size > 8 * 1024 * 1024) {
        showImportError('文件超过 8 MB；请改用数据库迁移或拆分导入');
        this.value = '';
        return;
      }
      var reader = new FileReader();
      reader.onload = function () {
        IMPORT.fileText = String(reader.result || '');
        IMPORT.fileLabel = f.name;
        $('#import-file-note').textContent = '已读取 ' + f.name + '（' + (f.size / 1024).toFixed(1) + ' KB）；解析后即从内存丢弃。';
      };
      reader.onerror = function () { showImportError('读取文件失败，请重试'); };
      reader.readAsText(f);
    });
    $('#import-preview').addEventListener('click', previewImport);
    $('#import-apply').addEventListener('click', applyImport);
  }

  function resetImport() {
    IMPORT.plan = null;
    $('#import-apply').disabled = true;
    $('#import-result').style.display = 'none';
    $('#import-error').style.display = 'none';
    $('#import-status').textContent = '未预览';
    $('#import-status').className = 'badge muted plain';
  }

  function showImportError(msg) {
    IMPORT.plan = null;
    $('#import-error-text').textContent = msg;
    $('#import-error').style.display = '';
    $('#import-result').style.display = 'none';
    $('#import-apply').disabled = true;
    $('#import-status').textContent = '解析失败';
    $('#import-status').className = 'badge warn plain';
  }

  function importBody() {
    if (IMPORT.mode === 'db') {
      var p = $('#import-db').value.trim();
      if (!p) { showImportError('请填写 x-ui.db 的完整路径'); return null; }
      return { db_path: p, label: p.split(/[\\/]/).pop() };
    }
    if (IMPORT.mode === 'file') {
      if (!IMPORT.fileText) { showImportError('请先选择 JSON 文件'); return null; }
      return { json_text: IMPORT.fileText, label: IMPORT.fileLabel || 'upload.json' };
    }
    var text = $('#import-json').value.trim();
    if (!text) { showImportError('请粘贴 3x-ui 导出的入站 JSON'); return null; }
    return { json_text: text, label: 'pasted' };
  }

  function previewImport() {
    var body = importBody();
    if (!body) return;
    $('#import-error').style.display = 'none';
    $('#import-preview').disabled = true;
    $('#import-status').textContent = '解析中…';
    $('#import-status').className = 'badge info plain';
    api('/api/migrate/preview', { method: 'POST', body: JSON.stringify(body) })
      .then(function (r) {
        IMPORT.plan = { body: body, preview: r };
        renderImportPlan(r);
        $('#import-status').textContent = '预览就绪 · ' + (r.source_kind === 'db' ? '数据库' : 'JSON') +
          (r.counts.already_imported ? ' · 已导入 ' + r.counts.already_imported : '');
        $('#import-status').className = 'badge ok plain';
        $('#import-apply').disabled = !(r.counts.import > 0);
      })
      .catch(function (e) { showImportError(e.message); })
      .then(function () { $('#import-preview').disabled = false; });
  }

  function renderImportPlan(r) {
    $('#import-result').style.display = '';
    $('#import-count-import').textContent = '导入 ' + r.counts.import;
    $('#import-count-skip').textContent = '跳过 ' + r.counts.skip +
      (r.counts.already_imported ? ' · 已导入 ' + r.counts.already_imported : '');
    $('#import-count-conflict').textContent = '冲突 ' + r.counts.conflict;
    $('#import-shape').textContent = (r.schema && r.schema.shape) || '—';
    var rows = (r.items || []).map(function (it) {
      var kind = it.kind === 'import' ? ['ok', '导入'] : (it.kind === 'conflict' ? ['warn', '冲突'] : ['muted', '跳过']);
      var m = it.mapped || {};
      return '<tr><td class="mono">' + esc(it.position || '—') + '</td>' +
        '<td>' + esc(it.remark || ('#' + it.source_id)) + '</td>' +
        '<td class="mono">' + esc(m.protocol || '—') + (m.network ? ' + ' + esc(m.network) : '') + '</td>' +
        '<td class="mono">' + esc(m.port == null ? '—' : m.port + '/' + (m.l4 || 'tcp')) + '</td>' +
        '<td><span class="badge ' + kind[0] + ' plain">' + kind[1] + '</span></td>' +
        '<td class="mono">' + esc((it.reasons || []).join('；') || '—') + '</td></tr>';
    }).join('');
    $('#import-tbody').innerHTML = rows || '<tr><td colspan="6"><div class="table-note">没有可导入的条目</div></td></tr>';
  }

  function applyImport() {
    if (!IMPORT.plan) return;
    var p = IMPORT.plan;
    var body = Object.assign({}, p.body, {
      preview_id: p.preview.preview_id, plan_hash: p.preview.plan_hash, confirm: true
    });
    $('#import-apply').disabled = true;
    $('#import-status').textContent = '写入中…';
    api('/api/migrate/apply', { method: 'POST', body: JSON.stringify(body) })
      .then(function (res) {
        if (res.demo) {
          toast('演示模式', res.message || '不会写入真实状态');
        } else {
          toast('导入完成', '新增 ' + res.imported + ' 条 · 跳过 ' + res.counts.skip + ' · 冲突 ' + res.counts.conflict);
        }
        IMPORT.plan = null;
        $('#import-status').textContent = res.demo ? '演示模式（未写入）' : '已导入 ' + res.imported + ' 条';
        $('#import-status').className = 'badge ok plain';
        renderImportPlan(Object.assign({}, p.preview, { counts: res.counts, items: res.items || p.preview.items }));
        if (!res.demo) loadAll();
      })
      .catch(function (e) {
        if (e.code === 'plan_changed') showImportError('预览已过期（状态发生变化），请重新预览');
        else showImportError(e.message);
      });
  }

  var clientCache = {};
  function prefetchClients(inbound) {
    if (clientCache[inbound.id]) return Promise.resolve(clientCache[inbound.id]);
    return api('/api/inbound/inbounds/' + encodeURIComponent(inbound.id) + '/clients').then(function (r) {
      clientCache[inbound.id] = r.items || [];
      return clientCache[inbound.id];
    });
  }
  function allClients() {
    var out = [];
    UI.inbounds.forEach(function (i) {
      var cs = clientCache[i.id] || [];
      cs.forEach(function (c) { out.push({ inbound: i, client: c }); });
    });
    return out;
  }

  /* ---------------- 新建向导 ---------------- */
  var wizardStep = 1;
  function openWizard() {
    var d = UI.defaults || {};
    var dflt = d.defaults || {};
    var sel = $('#wz-protocol');
    sel.innerHTML = (d.protocols || []).map(function (p) {
      return '<option value="' + esc(p.id) + '"' + (p.supported ? '' : ' disabled') + '>' + esc(p.label) + '</option>';
    }).join('');
    sel.value = dflt.protocol || 'vless-reality';
    $('#wz-dest').value = dflt.dest || 'www.microsoft.com:443';
    $('#wz-sni').value = (dflt.server_names || ['www.microsoft.com'])[0];
    $('#wz-port').value = dflt.port || 443;
    $('#wz-email').value = '';
    $('#wz-remark').value = '';
    $('#wz-listen').value = dflt.listen || '0.0.0.0';
    $('#wz-ports').innerHTML = (d.port_candidates || []).map(function (p) {
      return '<span class="chip" data-port="' + p.port + '">' + p.port + ' · ' + (p.free ? '可用' : '占用') + '</span>';
    }).join('');
    $$('#wz-ports .chip').forEach(function (c) {
      c.addEventListener('click', function () { $('#wz-port').value = c.dataset.port; updateWizardFields(); });
    });
    $('#wz-steps').innerHTML = (d.steps || []).map(function (s) {
      return '<span class="wz-step" data-step="' + s.no + '">' + s.no + '. ' + esc(s.title) + '</span>';
    }).join('');
    wizardStep = 1;
    $('#wz-error').hidden = true;
    $('#wizard-overlay').classList.add('open');
    updateWizardFields();
  }
  function closeWizard() { $('#wizard-overlay').classList.remove('open'); }
  function wizardProtocol() {
    var d = UI.defaults || {};
    return (d.protocols || []).filter(function (p) { return p.id === $('#wz-protocol').value; })[0] || {};
  }
  function wizardGo(delta) {
    if (delta > 0) {
      if (!$('#wz-remark').value.trim()) {
        wzError('请填写名称（remark）');
        return;
      }
      var port = Number($('#wz-port').value);
      if (!port || port < 1 || port > 65535) { wzError('端口必须在 1–65535'); return; }
    }
    wizardStep = Math.max(1, Math.min(6, wizardStep + delta));
    $('#wz-error').hidden = true;
    updateWizardFields();
  }
  function wzError(msg) {
    var e = $('#wz-error'); e.hidden = false; e.textContent = msg;
  }
  function updateWizardFields() {
    var p = wizardProtocol();
    $$('#wizard-steps .wz-step').forEach(function (s) {
      s.classList.toggle('on', Number(s.dataset.step) <= wizardStep);
    });
    var visibility = {
      1: ['#wz-remark', '#wz-protocol', '#wz-target'],
      2: ['#wz-listen', '#wz-port'],
      3: p.security === 'reality' ? ['#wz-dest', '#wz-sni'] : [],
      6: ['#wz-email']
    };
    $$('#wizard-overlay .field').forEach(function (f) {
      var control = f.querySelector('input,select');
      var show = true;
      if (control) {
        var id = '#' + control.id;
        var stepOf = null;
        Object.keys(visibility).forEach(function (k) { if (visibility[k].indexOf(id) >= 0) stepOf = Number(k); });
        if (stepOf !== null) show = stepOf === wizardStep;
        if ((id === '#wz-dest' || id === '#wz-sni') && p.security !== 'reality') show = false;
      }
      f.style.display = show ? '' : 'none';
    });
    $('#wz-ports').style.display = wizardStep === 2 ? '' : 'none';
    var port = Number($('#wz-port').value);
    $$('#wz-ports .chip').forEach(function (c) { c.classList.toggle('on', Number(c.dataset.port) === port); });
    var label = (UI.defaults && UI.defaults.steps || [])[wizardStep - 1];
    $('#wz-status').textContent = '第 ' + wizardStep + ' / 6 步 · ' + (label ? label.title : '');
    $('#wizard-commit').textContent = wizardStep === 6 ? '创建入站' : '下一步';
    $('#wizard-back').disabled = wizardStep === 1;
    if (!wizardStepHint) return;
    wizardStepHint.innerHTML = hintFor(wizardStep, p);
  }
  var wizardStepHint = null;
  function hintFor(step, p) {
    if (step === 4) {
      return p.security === 'reality'
        ? 'Reality：提交时生成 X25519 密钥对与 shortId；私钥只写入 state/state.json，API 只返回公钥标识。'
        : 'TLS：当前版本只记录自签证书路径占位，不生成证书、不写系统目录。';
    }
    if (step === 5) return '防火墙：当前版本不写宿主规则，只在事务摘要里标 skipped；需要开放端口请自行在云安全组处理。';
    if (step === 6) return '提交后写入 state/state.json（原子替换 + .bak），并写审计事件 inbound.wizard.committed。';
    return '';
  }
  function ensureHintBox() {
    if (wizardStepHint) return;
    wizardStepHint = document.createElement('div');
    wizardStepHint.className = 'wz-hint';
    var err = $('#wz-error');
    err.parentNode.insertBefore(wizardStepHint, err);
  }
  function commitWizard() {
    ensureHintBox();
    var p = wizardProtocol();
    var port = Number($('#wz-port').value);
    var body = {
      remark: $('#wz-remark').value.trim(),
      protocol: p.protocol || 'vless',
      network: p.network || 'tcp',
      security: p.security || 'reality',
      listen: $('#wz-listen').value,
      port: port,
      server_kernel: p.kernel || 'xray',
      target: $('#wz-target').value,
      reality: p.security === 'reality' ? {
        dest: $('#wz-dest').value.trim(),
        server_names: [$('#wz-sni').value.trim()].filter(Boolean),
        fingerprint: 'chrome',
        spider_x: '/'
      } : null,
      client: { email: $('#wz-email').value.trim(), flow: 'xtls-rprx-vision' },
      allow_unverified: true
    };
    $('#wizard-commit').disabled = true;
    api('/api/inbound/inbounds', { method: 'POST', body: JSON.stringify(body) }).then(function (r) {
      $('#wizard-commit').disabled = false;
      closeWizard();
      clientCache = {};
      toast('入站已创建', (r.item && r.item.remark || body.remark) + ' · ' + (r.warnings && r.warnings[0] ? r.warnings[0] : '已写入状态'));
      return api('/api/inbound/inbounds').then(function (list) {
        UI.inbounds = list.items || [];
        renderChrome(); renderOverview(); renderInbounds(); renderNodes();
        showPage('inbound');
      });
    }).catch(function (e) {
      $('#wizard-commit').disabled = false;
      var extra = (e.fields || []).map(function (f) { return f.field + '：' + f.message; }).join('\n');
      wzError(e.message + (extra ? '\n' + extra : ''));
    });
  }

  /* ---------------- 导出：规则集 ---------------- */
  function initExportDefaults() {
    UI.export.rules = {};
    UI.export.order = [];
    applyPreset(UI.export.preset);
    $('#export-preset-tabs').innerHTML = (UI.rules.presets || []).map(function (p) {
      return '<button data-preset="' + esc(p.id) + '"' + (p.id === UI.export.preset ? ' class="on active"' : '') + '>' + esc(p.label) + '</button>';
    }).join('');
    $$('#export-preset-tabs button').forEach(function (b) {
      b.addEventListener('click', function () {
        UI.export.preset = b.dataset.preset;
        $$('#export-preset-tabs button').forEach(function (x) { x.classList.toggle('active', x === b); x.classList.toggle('on', x === b); });
        applyPreset(UI.export.preset);
      });
    });
  }

  function catalog() { return (UI.rules && UI.rules.catalog) || []; }
  function rsFind(id) { return catalog().filter(function (r) { return r.id === id; })[0]; }
  function smartAction(id) {
    if (id.indexOf('private') >= 0 || id === 'geosite:cn' || id === 'geoip:cn') return 'direct';
    if (id.indexOf('category-ads-all') >= 0 || id.indexOf('category-porn') >= 0) return 'reject';
    return 'proxy';
  }
  function presetItems(id) {
    var p = ((UI.rules && UI.rules.presets) || []).filter(function (x) { return x.id === id; })[0];
    return (p && p.items) || [];
  }
  function applyPreset(id) {
    UI.export.rules = {}; UI.export.order = [];
    presetItems(id).forEach(function (it) {
      if (UI.export.rules[it.id] === undefined) UI.export.order.push(it.id);
      UI.export.rules[it.id] = it.action;
    });
    syncRulesetUI();
  }
  function addPresetItems(id, conflicts) {
    presetItems(id).forEach(function (it) {
      var cur = UI.export.rules[it.id];
      if (cur === undefined || cur === 'off') {
        if (UI.export.order.indexOf(it.id) < 0) UI.export.order.push(it.id);
        UI.export.rules[it.id] = it.action;
      } else if (cur !== it.action) {
        conflicts.push({ id: it.id, kept: cur, dropped: it.action });
      }
    });
  }

  function renderRulesets() {
    var box = $('#export-rule-groups');
    if (!box) return;
    var q = ($('#ruleset-search') ? $('#ruleset-search').value : '').toLowerCase();
    var onlyOn = $('#ruleset-only-on') && $('#ruleset-only-on').checked;
    var groups = (UI.rules && UI.rules.groups) || [];
    var acts = (UI.rules && UI.rules.actions) || [];
    var html = '';
    groups.forEach(function (g) {
      var items = catalog().filter(function (r) { return r.group === g; });
      var shown = items.filter(function (r) {
        var on = UI.export.rules[r.id] && UI.export.rules[r.id] !== 'off';
        if (onlyOn && !on) return false;
        return !q || (r.display + ' ' + r.real_id + ' ' + r.id).toLowerCase().indexOf(q) >= 0;
      });
      if (!shown.length) return;
      var onN = items.filter(function (r) { return UI.export.rules[r.id] && UI.export.rules[r.id] !== 'off'; }).length;
      html += '<details class="rs-group" open><summary><b>' + esc(g) + '</b><span class="rs-gcount">已选 ' + onN + ' / 共 ' + items.length + '</span></summary><div class="rs-rows">';
      shown.forEach(function (r) {
        var on = UI.export.rules[r.id] && UI.export.rules[r.id] !== 'off';
        var idx = UI.export.order.indexOf(r.id) + 1;
        var size = UI.export.format === 'singbox' ? r.srs_bytes : r.mihomo_bytes;
        var cached = cacheFor(r.id, UI.export.format);
        var srcLabel = cached ? ('← ' + sourceLabel(cached.source) + (cached.matches_baseline ? ' · 已校验' : ' · 基准已更新')) : '← 官方源（未缓存）';
        var cacheLine = cached
          ? '<div>缓存：<span class="mono">' + esc(cached.path) + '</span> · ' + fmtBytes(cached.size) + ' · ' + esc(cached.saved_at) + '</div>' +
            '<div>SHA-256：<span class="mono">' + esc((cached.sha256 || '').slice(0, 32)) + '…</span></div>'
          : '<div>缓存：尚未下载（导出时客户端仍会指向官方 URL）</div>';
        html += '<div class="rs-row" data-rule-id="' + esc(r.id) + '">' +
          '<span class="rs-idx">' + (on ? idx : '—') + '</span>' +
          '<div class="rs-main"><b>' + esc(r.display) + '</b><span class="rs-id">' + esc(r.real_id) + '</span><span class="rs-meta">' + fmtBytes(size) + '</span><span class="rs-src">' + srcLabel + '</span></div>' +
          '<select class="select rs-act" data-rule="' + esc(r.id) + '">' + acts.map(function (a) {
            return '<option value="' + a.id + '"' + ((UI.export.rules[r.id] || 'off') === a.id ? ' selected' : '') + '>' + esc(a.label) + '</option>';
          }).join('') + '</select>' +
          '<details class="rs-detail"><summary>详情</summary><div class="rs-detail-body"><div>官方：<span class="mono">https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat</span></div>' + cacheLine + '<div>校验：镜像内容须与官方 SHA-256 基准一致才落盘</div></div></details>' +
          '</div>';
      });
      html += '</div></details>';
    });
    box.innerHTML = html || '<div class="muted" style="padding:10px">没有匹配的规则集</div>';
    $$('.rs-act', box).forEach(function (s) {
      s.addEventListener('change', function () {
        var id = s.dataset.rule;
        if (s.value === 'off') {
          UI.export.rules[id] = 'off';
          UI.export.order = UI.export.order.filter(function (x) { return x !== id; });
        } else {
          if (UI.export.order.indexOf(id) < 0) UI.export.order.push(id);
          UI.export.rules[id] = s.value;
        }
        syncRulesetUI();
      });
    });
    syncRulesetUI();
  }

  function syncRulesetUI() {
    var enabled = Object.keys(UI.export.rules).filter(function (k) { return UI.export.rules[k] !== 'off'; });
    var vol = enabled.reduce(function (a, id) {
      var r = rsFind(id); if (!r) return a;
      return a + (UI.export.format === 'singbox' ? r.srs_bytes : r.mihomo_bytes);
    }, 0);
    $('#export-ruleset-count').textContent = '已启用 ' + enabled.length + ' / 共 ' + catalog().length;
    $('#export-preset-summary').textContent = UI.export.preset ? '已组合：' + (UI.rules.presets.filter(function (p) { return p.id === UI.export.preset; })[0] || {}).label : '未组合';
    $('#export-preset-volume').textContent = '合计 ' + fmtBytes(vol) + ' · ' + enabled.length + ' 条（去重后）';
    $('#export-preset-state').textContent = UI.export.preset ? '预设' : '手动';
    var reasons = presetItems(UI.export.preset).map(function (it) {
      var r = rsFind(it.id);
      return r ? '<li><b>' + esc(r.display) + '</b>（' + esc(it.action) + '）← 组合预设：' + esc(((UI.rules.presets || []).filter(function (p) { return p.id === UI.export.preset; })[0] || {}).why || '') + '</li>' : '';
    }).join('');
    $('#export-preset-reasons').innerHTML = reasons;
    var same = true;
    presetItems(UI.export.preset).forEach(function (it) {
      if ((UI.export.rules[it.id] || 'off') !== it.action) same = false;
    });
    if (Object.keys(UI.export.rules).filter(function (k) { return UI.export.rules[k] !== 'off'; }).length !== presetItems(UI.export.preset).length) same = false;
    $('#export-preset-diff').textContent = same ? '与组合预设一致' : '与组合预设不同（手动调整）';
    schedulePreview();
  }

  function bindExportEvents() {
    $$('#export-format-tabs button').forEach(function (b) {
      b.addEventListener('click', function () {
        $$('#export-format-tabs button').forEach(function (x) { x.classList.toggle('active', x === b); });
        UI.export.format = b.dataset.exportFormat;
        renderRulesets();
      });
    });
    $$('#export-scenario-tabs button').forEach(function (b) {
      b.addEventListener('click', function () {
        $$('#export-scenario-tabs button').forEach(function (x) { x.classList.toggle('active', x === b); });
        UI.export.scenario = b.dataset.exportScenario;
        $('#export-preview').dataset.scenario = UI.export.scenario;
        schedulePreview();
      });
    });
    $('#export-version').addEventListener('change', function () { UI.export.bucket = this.value; schedulePreview(); });
    $('#ruleset-search').addEventListener('input', debounce(renderRulesets, 180));
    $('#ruleset-only-on').addEventListener('change', renderRulesets);
    $('#export-preset-restore').addEventListener('click', function () { applyPreset(UI.export.preset); });
    $('#export-preset-clear').addEventListener('click', function () {
      UI.export.rules = {}; UI.export.order = [];
      syncRulesetUI(); renderRulesets();
    });
    $('#ruleset-select-all').addEventListener('click', function () {
      catalog().forEach(function (r) { UI.export.rules[r.id] = smartAction(r.id); });
      UI.export.order = catalog().map(function (r) { return r.id; });
      syncRulesetUI(); renderRulesets();
    });
    $('#ruleset-invert').addEventListener('click', function () {
      catalog().forEach(function (r) {
        var cur = UI.export.rules[r.id] || 'off';
        UI.export.rules[r.id] = cur === 'off' ? smartAction(r.id) : 'off';
      });
      UI.export.order = catalog().filter(function (r) { return UI.export.rules[r.id] !== 'off'; }).map(function (r) { return r.id; });
      syncRulesetUI(); renderRulesets();
    });
    $('#ruleset-clear').addEventListener('click', function () {
      UI.export.rules = {}; UI.export.order = [];
      syncRulesetUI(); renderRulesets();
    });
    $('#ruleset-reorder').addEventListener('click', function () {
      UI.export.order = catalog().filter(function (r) { return UI.export.rules[r.id] !== 'off'; }).map(function (r) { return r.id; });
      renderRulesets();
    });
    $('#export-search').addEventListener('input', debounce(renderNodes, 160));
    $('#export-filter-proto').addEventListener('change', renderNodes);
    $('#export-filter-kernel').addEventListener('change', renderNodes);
    $('#export-select-all').addEventListener('click', function () {
      allClients().forEach(function (x) { if (x.client.enabled && x.inbound.enabled) UI.export.nodeSel[x.client.id] = true; });
      renderNodes(); schedulePreview();
    });
    $('#export-invert').addEventListener('click', function () {
      allClients().forEach(function (x) { if (x.client.enabled && x.inbound.enabled) UI.export.nodeSel[x.client.id] = !UI.export.nodeSel[x.client.id]; });
      renderNodes(); schedulePreview();
    });
    $('#export-clear').addEventListener('click', function () { UI.export.nodeSel = {}; renderNodes(); schedulePreview(); });
    $('#export-validate').addEventListener('click', function () { schedulePreview(true); });
    $('#export-warn-ack').addEventListener('change', function () { UI.export.ack = this.checked; updateExportActions(); });
    $('#export-copy').addEventListener('click', copyConfig);
    $('#export-download').addEventListener('click', downloadConfig);
    $('#export-sub').addEventListener('click', issueToken);
    $('#export-token-new').addEventListener('click', issueToken);
    $('#export-sub-url').addEventListener('click', function () {
      copyText($('#export-sub-url').textContent).then(function () { toast('已复制订阅链接', '同一 render_id 的配置'); });
    });
  }

  function debounce(fn, ms) {
    var t = null;
    return function () {
      clearTimeout(t); t = setTimeout(fn, ms);
    };
  }

  var nodeClients = [];
  function renderNodes() {
    var box = $('#export-node-list');
    if (!box) return;
    var need = UI.inbounds.filter(function (i) { return !clientCache[i.id]; });
    if (need.length) {
      Promise.all(need.map(prefetchClients)).then(function () {
        nodeClients = allClients();
        renderNodes();
      }).catch(function () {});
      return;
    }
    nodeClients = allClients();
    if (((UI.panel && UI.panel.demo) || DEMO) && selectedClientIds().length === 0) {
      nodeClients.forEach(function (x) { if (x.client.enabled && x.inbound.enabled) UI.export.nodeSel[x.client.id] = true; });
    }
    var q = ($('#export-search').value || '').toLowerCase();
    var fp = $('#export-filter-proto').value || 'all';
    var fk = $('#export-filter-kernel').value || 'all';
    var list = UI.inbounds.filter(function (i) {
      return (fp === 'all' || i.protocol === fp) && (fk === 'all' || i.server_kernel === fk) &&
        (!q || (i.remark + ' ' + i.tag + ' ' + nodeClients.filter(function (x) { return x.inbound.id === i.id; }).map(function (x) { return x.client.email; }).join(' ')).toLowerCase().indexOf(q) >= 0);
    });
    box.innerHTML = list.map(function (i) {
      var cs = clientCache[i.id] || [];
      var n = cs.filter(function (c) { return UI.export.nodeSel[c.id]; }).length;
      var rows = cs.map(function (c) {
        return '<label class="client-row"><input type="checkbox" class="js-client-check" data-client="' + esc(c.id) + '" data-inbound="' + esc(i.id) + '"' +
          (UI.export.nodeSel[c.id] ? ' checked' : '') + (c.enabled && i.enabled ? '' : ' disabled') + '> ' + esc(c.email) + (c.enabled ? '' : '（已停用）') + '</label>';
      }).join('');
      return '<div class="node-row" data-proto="' + esc(i.protocol) + '" data-kernel="' + esc(i.server_kernel) + '">' +
        '<div class="node-row-main"><input type="checkbox" class="js-inbound-check" data-inbound="' + esc(i.id) + '"' + (n === cs.length && n > 0 ? ' checked' : '') + '>' +
        '<b>' + esc(i.remark) + '</b><span class="badge ' + (i.server_kernel === 'xray' ? 'info' : 'accent') + ' plain">' + esc(i.server_kernel) + '</span>' +
        '<span class="badge muted plain">' + n + '/' + cs.length + ' 客户端</span>' +
        (i.enabled ? '<span class="badge warn plain">未验证</span>' : '<span class="badge muted plain">停用</span>') + '</div>' +
        '<div class="client-list">' + rows + '</div></div>';
    }).join('') || '<div class="table-note">没有匹配的入站</div>';

    $$('.js-client-check', box).forEach(function (cb) {
      cb.addEventListener('change', function () {
        UI.export.nodeSel[cb.dataset.client] = cb.checked;
        updateNodeCounts(); schedulePreview();
      });
    });
    $$('.js-inbound-check', box).forEach(function (cb) {
      cb.addEventListener('change', function () {
        (clientCache[cb.dataset.inbound] || []).forEach(function (c) {
          if (c.enabled) UI.export.nodeSel[c.id] = cb.checked;
        });
        renderNodes(); schedulePreview();
      });
    });
    updateNodeCounts();
    if ((UI.panel && UI.panel.demo) || DEMO) schedulePreview();
  }

  function updateNodeCounts() {
    var ids = Object.keys(UI.export.nodeSel).filter(function (k) { return UI.export.nodeSel[k]; });
    var inboundIds = {};
    nodeClients.forEach(function (x) { if (UI.export.nodeSel[x.client.id]) inboundIds[x.inbound.id] = true; });
    $('#export-count').textContent = '已选 ' + ids.length + ' / 共 ' + nodeClients.length;
    $('#export-detail').textContent = Object.keys(inboundIds).length + ' 节点 · ' + ids.length + ' 客户端';
    $('#export-empty-hint').style.display = ids.length ? 'none' : '';
  }

  function selectedClientIds() {
    return Object.keys(UI.export.nodeSel).filter(function (k) { return UI.export.nodeSel[k]; });
  }

  function schedulePreview(force) {
    clearTimeout(UI.export.timer);
    UI.export.timer = setTimeout(function () { loadPreview(force); }, force ? 0 : 260);
  }

  function loadPreview(force) {
    var ids = selectedClientIds();
    if (!ids.length) {
      renderPreviewEmpty();
      return;
    }
    var inboundIds = [];
    nodeClients.forEach(function (x) { if (UI.export.nodeSel[x.client.id] && inboundIds.indexOf(x.inbound.id) < 0) inboundIds.push(x.inbound.id); });
    var rules = Object.keys(UI.export.rules).filter(function (k) { return UI.export.rules[k] !== 'off'; }).map(function (k) { return k + '@' + UI.export.rules[k]; });
    var q = '/api/export/preview?format=' + encodeURIComponent(UI.export.format) +
      '&scenario=' + encodeURIComponent(UI.export.scenario) +
      '&bucket=' + encodeURIComponent(UI.export.bucket) +
      '&inbound_ids=' + encodeURIComponent(inboundIds.join(',')) +
      '&client_ids=' + encodeURIComponent(ids.join(',')) +
      '&rules=' + encodeURIComponent(rules.join(','));
    api(q).then(function (r) {
      UI.export.preview = r;
      renderPreview(r);
    }).catch(function (e) {
      toast('预览失败', e.message);
    });
  }

  function renderPreviewEmpty() {
    UI.export.preview = null;
    $('#export-file-name').textContent = 'config.yaml';
    $('#export-size').textContent = '—';
    $('#export-preview-sum').textContent = '至少选择 1 个客户端';
    $$('#export-preview [data-sec]').forEach(function (p) { p.textContent = ''; });
    $('#export-validation-badge') && ($('#export-validate-badge').textContent = '未校验');
    $('#export-validate-status').textContent = '未执行';
    ['l0', 'l1', 'l2'].forEach(function (k) { setLayer(k, '待执行'); });
    updateExportActions();
  }

  function renderPreview(r) {
    $('#export-file-name').textContent = r.file_name;
    $('#export-size').textContent = fmtBytes(r.bytes);
    $('#export-preview-sum').textContent = (r.format === 'singbox' ? 'sing-box JSON' : 'mihomo YAML') + ' · ' + r.bucket + ' 档 · ' + r.entries + ' 条 · hash ' + r.content_hash.slice(0, 12);
    $('#export-render-note').textContent = '按 ' + (r.format === 'singbox' ? 'sing-box' : 'mihomo') + ' ' + r.bucket + ' 档渲染';
    $('#export-meta-rid').textContent = r.render_id;
    $('#export-meta-hash').textContent = 'hash ' + r.content_hash.slice(0, 16) + '…';
    $('#export-meta-time').textContent = new Date().toLocaleTimeString();
    $$('#export-preview [data-sec]').forEach(function (p) {
      p.textContent = (r.sections && r.sections[p.dataset.sec]) || '';
    });
    var v = r.validation || {};
    var overall = v.overall || 'WARN';
    $('#export-validate-badge').textContent = overall;
    $('#export-validate-badge').className = 'badge ' + badgeClass(overall) + ' plain';
    $('#export-validate-status').textContent = overall;
    (v.layers || []).forEach(function (layer) {
      var key = String(layer.layer || '').toLowerCase();
      var extra = layer.status === 'UNVERIFIED' ? '（未执行二进制校验）' : '';
      setLayer(key, layer.status + extra);
    });
    var fail = (v.layers || []).filter(function (l) { return l.status === 'FAIL'; });
    var failBox = $('#export-fail-reason');
    if (fail.length) {
      failBox.style.display = '';
      var items = fail.reduce(function (a, l) { return a.concat(l.items || []); }, []);
      failBox.innerHTML = '<span><b>导出被拦截：</b>' + items.map(function (i) { return esc(i.message || i.code || ''); }).join('；') + '</span>';
    } else {
      failBox.style.display = 'none';
    }
    var ackWrap = $('#export-warn-ack-wrap');
    if (overall === 'WARN') {
      ackWrap.style.display = '';
      if (!UI.export.ack) { $('#export-warn-ack').checked = false; }
    } else {
      ackWrap.style.display = 'none';
      UI.export.ack = true;
      $('#export-warn-ack').checked = true;
    }
    $('#export-warning').style.display = r.nodes.some(function (n) { return n.xhttp; }) ? '' : 'none';
    updateExportActions();
    if (UI.panel && UI.panel.demo) $('#export-meta-time').textContent += ' · 演示数据';
  }

  function setLayer(key, text) {
    var el = $('#export-layer-' + key + '-badge');
    if (!el) return;
    el.textContent = text;
    el.className = 'badge ' + (text.indexOf('FAIL') >= 0 ? 'warn' : text.indexOf('PASS') >= 0 ? 'ok' : 'muted') + ' plain';
  }

  function updateExportActions() {
    var r = UI.export.preview;
    var disabled = !r || r.validation.overall === 'FAIL' || (r.validation.overall === 'WARN' && !UI.export.ack);
    ['#export-copy', '#export-download', '#export-sub', '#export-token-new'].forEach(function (s) {
      var b = $(s); if (b) b.disabled = disabled;
    });
  }

  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) return navigator.clipboard.writeText(text);
    return new Promise(function (resolve, reject) {
      var ta = document.createElement('textarea');
      ta.value = text; document.body.appendChild(ta); ta.select();
      try { document.execCommand('copy'); resolve(); } catch (e) { reject(e); }
      document.body.removeChild(ta);
    });
  }

  function copyConfig() {
    if (!UI.export.preview) return;
    copyText(UI.export.preview.content).then(function () {
      toast('已复制完整配置', UI.export.preview.render_id + ' · 与下载/订阅同源');
    }).catch(function (e) { toast('复制失败', e.message); });
  }

  function downloadConfig() {
    if (!UI.export.preview) return;
    var blob = new Blob([UI.export.preview.content], { type: 'text/plain;charset=utf-8' });
    var a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = UI.export.preview.file_name;
    document.body.appendChild(a); a.click(); document.body.removeChild(a);
    setTimeout(function () { URL.revokeObjectURL(a.href); }, 1500);
    toast('已开始下载', UI.export.preview.file_name + ' · ' + UI.export.preview.render_id);
  }

  function issueToken() {
    var ids = selectedClientIds();
    if (!ids.length) { toast('先选择客户端', '订阅令牌按客户端签发'); return; }
    api('/api/export/tokens', { method: 'POST', body: JSON.stringify({ client_id: ids[0], target: UI.export.format === 'singbox' ? 'singbox' : 'clash' }) })
      .then(function (r) {
        var base = (UI.panel && UI.panel.panel_address) ? ('http://' + UI.panel.panel_address) : location.origin;
        $('#export-sub-url').textContent = (DEMO ? '' : base) + r.url;
        var target = UI.export.format === 'singbox' ? 'sing-box' : 'mihomo';
        var row = '<div class="token-row" data-token-id="' + esc(r.id) + '"><span class="mono mask">' + esc(r.token) + '</span>' +
          '<span class="badge ' + (target === 'sing-box' ? 'purple' : 'info') + ' plain">' + target + '</span>' +
          '<div class="row-actions"><button class="btn btn-sm js-rotate-token">轮换</button><button class="btn btn-sm js-revoke-token">吊销</button></div></div>';
        $('#export-tokens').innerHTML = row + '<div class="table-note">' + esc(r.note || '只展示一次；服务端仅存 SHA-256') + '</div>';
        toast('订阅令牌已生成', r.url);
      }).catch(function (e) { toast('签发失败', e.message); });
  }

  // 设计稿里的“轮换/吊销”按钮：只操作带 data-token-id 的真实令牌行（静态演示行保持占位）。
  function bindTokenActions() {
    document.addEventListener('click', function (e) {
      var row = e.target && e.target.closest ? e.target.closest('[data-token-id]') : null;
      if (!row || DEMO) return;
      var id = row.getAttribute('data-token-id');
      if (e.target.closest('.js-rotate-token')) {
        api('/api/export/tokens/' + encodeURIComponent(id) + '/rotate', { method: 'POST' })
          .then(function (r) {
            row.querySelector('.mask').textContent = r.token;
            toast('令牌已轮换', '旧链接立即失效；新 token 仅展示一次');
          }).catch(function (err) { toast('轮换失败', err.message); });
      } else if (e.target.closest('.js-revoke-token')) {
        if (!window.confirm('吊销后该订阅立即失效，且不可撤销。确定吊销？')) return;
        api('/api/export/tokens/' + encodeURIComponent(id) + '/revoke', { method: 'POST' })
          .then(function () {
            row.querySelector('.row-actions').innerHTML = '<span class="badge bad plain">已吊销</span>';
            toast('令牌已吊销', '该订阅链接立即失效');
          }).catch(function (err) { toast('吊销失败', err.message); });
      }
    });
  }

  /* ---------------- 镜像源（M2） ---------------- */
  function cacheFor(id, format) {
    var items = ((UI.rules || {}).cache || {}).items || [];
    for (var i = 0; i < items.length; i++) if (items[i].key === format + ':' + id) return items[i];
    return null;
  }

  function sourceLabel(id) {
    var list = (UI.sources && UI.sources.sources) || [];
    for (var i = 0; i < list.length; i++) if (list[i].id === id) return list[i].label;
    return id;
  }

  function statusText(s) {
    if (!s.enabled) return '<span class="mirror-status off">已禁用</span>';
    if (!s.last) return '<span class="mirror-status off">未测试</span>';
    var l = s.last;
    if (l.status === 'ok') return '<span class="mirror-status">正常 · ' + l.ms + ' ms</span>';
    var txt = l.status === 'hash_mismatch' ? '校验失败' : (l.status === 'no_baseline' ? '缺基准' : '失败');
    return '<span class="mirror-status bad" title="' + esc(l.detail || '') + '">' + txt + ' · ' + l.ms + ' ms</span>';
  }

  function renderMirrorCard() {
    if (!UI.sources) return;
    var srcs = UI.sources.sources || [];
    var on = srcs.filter(function (s) { return s.enabled; }).length;
    if ($('#mirror-count')) $('#mirror-count').textContent = on + ' / ' + srcs.length + ' 已启用';

    var modeSel = $('#mirror-mode');
    if (modeSel) {
      modeSel.innerHTML = '<option value="auto">自动回退（镜像优先，官方兜底）</option>' +
        srcs.filter(function (s) { return s.enabled; }).map(function (s) {
          return '<option value="' + esc(s.id) + '">手动 · ' + esc(s.label) + '</option>';
        }).join('');
      var want = UI.sources.mode || 'auto';
      modeSel.value = want;
      if (modeSel.value !== want) modeSel.value = 'auto';
    }
    var cur = $('#mirror-current');
    if (cur) cur.textContent = '当前源：' + (UI.sources.mode === 'auto' || !UI.sources.mode ? '自动回退' : sourceLabel(UI.sources.mode));

    var idSel = $('#ruleset-fetch-id');
    if (idSel) {
      var catalog = (UI.rules && UI.rules.catalog) || [];
      var prev = idSel.value || 'geoip:private';
      idSel.innerHTML = catalog.map(function (r) {
        return '<option value="' + esc(r.id) + '">' + esc(r.display) + ' · ' + esc(r.id) + '</option>';
      }).join('');
      if (catalog.some(function (r) { return r.id === prev; })) idSel.value = prev;
    }

    var box = $('#mirror-list');
    if (!box) return;
    box.innerHTML = srcs.map(function (s) {
      var badge = s.locked ? '<span class="badge accent">官方</span>'
        : (s.enabled ? '<span class="badge warn">第三方</span>' : '<span class="badge muted">未启用</span>');
      var ro = s.locked ? ' readonly' : '';
      return '<tr data-mirror-id="' + esc(s.id) + '" class="' + (s.enabled ? '' : 'mirror-row-off') + '">' +
        '<td><div class="cell-main"><b>' + esc(s.label) + '</b>' + badge + '</div><div class="table-note">' + esc(s.note || '') + '</div></td>' +
        '<td><input class="mirror-url" data-url="' + esc(s.id) + '" value="' + esc(s.url_mihomo) + '" aria-label="镜像 URL"' + ro + '></td>' +
        '<td><input class="mirror-priority" data-priority="' + esc(s.id) + '" value="' + s.priority + '" aria-label="优先级"' + ro + '></td>' +
        '<td><button class="switch ' + (s.enabled ? 'on' : '') + '" role="switch" aria-checked="' + (s.enabled ? 'true' : 'false') + '" data-toggle="' + esc(s.id) + '" aria-label="启用 ' + esc(s.label) + '"' + (s.locked ? ' disabled' : '') + '></button></td>' +
        '<td>' + statusText(s) + '</td>' +
        '<td><div class="row-actions"><button class="btn btn-sm" data-test="' + esc(s.id) + '">测速</button>' +
        (s.locked ? '' : '<button class="btn btn-sm" data-del="' + esc(s.id) + '">删除</button>') + '</div></td></tr>';
    }).join('') || '<tr><td colspan="6" class="muted">没有配置任何源</td></tr>';

    renderFetchResult(UI.lastFetch);
  }

  function renderFetchResult(r) {
    var box = $('#mirror-result');
    if (!box) return;
    if (!r) {
      box.innerHTML = '<div class="muted">尚未拉取。点「拉取并校验」会展示每一步的源、耗时与校验结论。</div>';
      return;
    }
    var head = r.ok
      ? '<span class="badge ok">通过</span> <b>' + esc(r.id) + '</b> 由 <b>' + esc((r.chosen || {}).label || '') + '</b> 提供 · 总耗时 ' + r.total_ms + ' ms · ' + fmtBytes((r.chosen || {}).bytes || 0)
      : '<span class="badge warn">未通过</span> <b>' + esc(r.id || '') + '</b> · 未落盘';
    var lines = (r.attempts || []).map(function (a) {
      var cls = a.status === 'ok' ? 'ok' : 'bad';
      var tail = a.status === 'ok'
        ? (a.bytes ? fmtBytes(a.bytes) + ' · ' : '') + (a.role === 'baseline' ? '官方基准' : '匹配官方基准')
        : esc(a.detail || a.status);
      return '<div class="mirror-attempt"><b>' + esc(a.label || a.source) + '</b><span class="' + cls + '">' + esc(a.status) + '</span><span>' + a.ms + ' ms</span><span class="muted">' + tail + '</span></div>';
    }).join('');
    if (!lines && r.error) lines = '<div class="mirror-attempt"><span class="bad">' + esc(r.error.code) + '</span><span>' + esc(r.error.message) + '</span></div>';
    var base = r.baseline
      ? '<div class="muted">官方基准 SHA-256：<span class="mono">' + esc(r.baseline.sha256.slice(0, 32)) + '…</span> · ' + esc(r.baseline.fetched_at) + '</div>'
      : '<div class="muted">尚无官方基准</div>';
    box.innerHTML = '<div style="margin-bottom:6px">' + head + '</div>' + (lines || '<div class="muted">没有尝试记录</div>') + base + '<div class="muted" style="margin-top:6px">' + esc(r.note || '') + '</div>';
  }

  function refreshSources() {
    return api('/api/rulesets/sources').then(function (s) {
      UI.sources = s;
      renderMirrorCard();
      return s;
    });
  }

  function runRulesetFetch(refreshBaseline) {
    var id = $('#ruleset-fetch-id') ? $('#ruleset-fetch-id').value : 'geoip:private';
    var format = $('#ruleset-fetch-format') ? $('#ruleset-fetch-format').value : 'mihomo';
    var mode = $('#mirror-mode') ? $('#mirror-mode').value : 'auto';
    var btn = refreshBaseline ? $('#mirror-refresh-baseline') : $('#ruleset-fetch-run');
    if (btn) btn.disabled = true;
    var body = { id: id, format: format, source: refreshBaseline ? 'official' : (mode || 'auto'), refresh_baseline: !!refreshBaseline };
    api('/api/rulesets/fetch', { method: 'POST', body: JSON.stringify(body) })
      .then(function (r) {
        UI.lastFetch = r;
        renderFetchResult(r);
        toast(r.ok ? '拉取并校验通过' : '拉取未通过', r.ok ? ((r.chosen || {}).label + ' · ' + r.total_ms + ' ms') : '详见回退链');
        return Promise.all([refreshSources(), api('/api/rulesets')]);
      }).then(function (rs) {
        UI.rules = rs[1];
        renderRulesets();
        renderOverview();
      }).catch(function (e) { toast('拉取失败', e.message); })
      .then(function () { if (btn) btn.disabled = false; });
  }

  function bindMirrorEvents() {
    var list = $('#mirror-list');
    if (!list) return;
    list.addEventListener('click', function (e) {
      var t = e.target.closest('button');
      if (!t) return;
      if (t.dataset.toggle) {
        var sid = t.dataset.toggle;
        var cur = (UI.sources.sources.filter(function (s) { return s.id === sid; })[0] || {}).enabled;
        api('/api/rulesets/sources/' + encodeURIComponent(sid), { method: 'PATCH', body: JSON.stringify({ enabled: !cur }) })
          .then(refreshSources).catch(function (err) { toast('修改失败', err.message); });
      } else if (t.dataset.test) {
        var id = t.dataset.test;
        t.disabled = true;
        api('/api/rulesets/sources/test', { method: 'POST', body: JSON.stringify({ id: id }) })
          .then(function (r) {
            toast(r.ok ? '测速通过' : '测速失败', (r.ok ? (r.ms + ' ms · ' + fmtBytes(r.bytes || 0)) : r.detail) + ' · ' + (r.detail || ''));
            return refreshSources();
          }).catch(function (err) { toast('测速失败', err.message); })
          .then(function () { t.disabled = false; });
      } else if (t.dataset.del) {
        if (!confirm('删除镜像「' + t.dataset.del + '」？')) return;
        api('/api/rulesets/sources/' + encodeURIComponent(t.dataset.del), { method: 'DELETE' })
          .then(refreshSources).catch(function (err) { toast('删除失败', err.message); });
      }
    });
    list.addEventListener('change', function (e) {
      var t = e.target;
      if (t.dataset.priority) {
        api('/api/rulesets/sources/' + encodeURIComponent(t.dataset.priority), { method: 'PATCH', body: JSON.stringify({ priority: Number(t.value) || 6 }) })
          .then(refreshSources).catch(function (err) { toast('修改失败', err.message); });
      } else if (t.dataset.url) {
        api('/api/rulesets/sources/' + encodeURIComponent(t.dataset.url), { method: 'PATCH', body: JSON.stringify({ url_mihomo: t.value }) })
          .then(refreshSources).catch(function (err) { toast('修改失败', err.message); });
      }
    });
    if ($('#mirror-mode')) $('#mirror-mode').addEventListener('change', function () {
      api('/api/rulesets/sources/mode', { method: 'PUT', body: JSON.stringify({ mode: this.value }) })
        .then(refreshSources).catch(function (err) { toast('切换失败', err.message); });
    });
    if ($('#mirror-add')) $('#mirror-add').addEventListener('click', function () {
      var label = ($('#mirror-name').value || '').trim();
      var url = ($('#mirror-url-input').value || '').trim();
      var srs = ($('#mirror-srs-input').value || '').trim();
      var pr = Number($('#mirror-priority-input').value) || 6;
      if (!label || !url) { toast('请填写名称和 URL 模板', '模板里可用 {kind} 与 {id} 占位'); return; }
      api('/api/rulesets/sources', { method: 'POST', body: JSON.stringify({ label: label, url_mihomo: url, url_singbox: srs, priority: pr, enabled: true }) })
        .then(function () { $('#mirror-name').value = ''; $('#mirror-url-input').value = ''; $('#mirror-srs-input').value = ''; return refreshSources(); })
        .then(function () { toast('镜像已添加', label); })
        .catch(function (err) { toast('添加失败', err.message); });
    });
    if ($('#ruleset-fetch-run')) $('#ruleset-fetch-run').addEventListener('click', function () { runRulesetFetch(false); });
    if ($('#mirror-refresh-baseline')) $('#mirror-refresh-baseline').addEventListener('click', function () { runRulesetFetch(true); });
  }

  /* ---------------- 内核 ---------------- */
  function renderKernels() {
    var box = $('#kernel-detect-body');
    if (!box || !UI.kernels) return;
    var bins = UI.kernels.binaries || [];
    $('#kernel-probed-at').textContent = '探测于 ' + (UI.kernels.probed_at || '') .replace('T', ' ').replace('Z', ' UTC');
    var rows = bins.map(function (b) {
      return '<div class="kv"><span class="k">' + esc(b.name) + '</span><span class="v plain">' +
        (b.present ? '存在 · ' + esc(b.version || '版本未知') + '\n' + esc(b.path) : '未找到') + '</span>' +
        '<span class="badge ' + (b.present ? 'ok' : 'muted') + ' plain">' + (b.present ? '已探测' : '缺失') + '</span></div>';
    }).join('');
    var matrix = '<div class="table-wrap" style="margin-top:12px"><table><thead><tr><th>能力</th><th>xray</th><th>sing-box</th><th>mihomo</th><th>状态</th></tr></thead><tbody>' +
      (UI.kernels.capabilities || []).map(function (c) {
        return '<tr><td><div class="cell-main"><b>' + esc(c.label) + '</b><span class="mono">' + esc(c.id) + '</span></div></td>' +
          '<td>' + esc(c.xray) + '</td><td>' + esc(c['sing-box']) + '</td><td>' + esc(c.mihomo) + '</td>' +
          '<td><span class="badge ' + (c.status === '部分核实' ? 'info' : 'warn') + ' plain">' + esc(c.status) + '</span></td></tr>';
      }).join('') + '</tbody></table></div>';
    var buckets = '<div class="table-wrap" style="margin-top:12px"><table><thead><tr><th>版本档</th><th>标签</th><th>是否建档</th><th>状态</th></tr></thead><tbody>' +
      (UI.kernels.buckets || []).map(function (b) {
        return '<tr><td class="mono">' + esc(b.bucket) + '</td><td>' + esc(b.label) + '</td><td>' + (b.built ? '是' : '<span class="badge warn plain">否</span>') + '</td><td>' + esc(b.status) + '</td></tr>';
      }).join('') + '</tbody></table></div>' + '<div class="table-note">' + esc(UI.kernels.policy && UI.kernels.policy.missing_binary || '') + '</div>';
    box.innerHTML = rows + matrix + buckets;
  }

  /* ---------------- 日志 ---------------- */
  function renderLogs() {
    var box = $('#log-stream');
    if (!box) return;
    var items = (UI.logs && UI.logs.items) || [];
    box.innerHTML = items.map(function (e) {
      var ts = String(e.ts || '').replace('T', ' ').replace('Z', '');
      var time = ts.split(' ')[1] || ts;
      return '<div class="log-line" data-level="audit"><span class="log-time">' + esc(time) + '</span><span class="log-lv audit">AUDIT</span><span class="log-msg">' + esc(e.event || '') + ' · ' + esc(JSON.stringify(e.detail || {})) + '</span></div>';
    }).join('') || '<div class="log-line"><span class="log-msg">暂无审计事件。创建入站或导出后会出现。</span></div>';
    var foot = $('.log-foot span');
    if (foot) foot.textContent = '显示 ' + items.length + ' 行';
  }

  /* ---------------- 设置 ---------------- */
  function renderSettings() {
    if (!UI.panel) return;
    $('#panel-address-input').value = UI.panel.panel_address || '';
    $('#panel-version-input').value = UI.panel.version || '—';
    var remote = UI.panel.remote_access || {};
    var listen = String(UI.panel.listen || '127.0.0.1:18080');
    var host = listen.slice(0, listen.lastIndexOf(':')) || '127.0.0.1';
    var port = listen.slice(listen.lastIndexOf(':') + 1) || '18080';
    var sel = $('#panel-listen');
    if (sel) sel.value = stateListenIsLoopback(host) ? '127.0.0.1' : '0.0.0.0';
    if ($('#panel-port')) $('#panel-port').value = port;
    if ($('#trusted-proxies')) $('#trusted-proxies').value = (remote.trusted_proxies || []).join(', ');
    var st = $('#remote-status');
    if (st) {
      if (remote.forced_loopback) {
        st.textContent = '⚠ ' + (remote.warning || '配置为非回环监听，但远程访问未确认，已强制回环');
      } else if (remote.enabled) {
        st.textContent = '远程访问已开启（确认于 ' + (remote.acknowledged_at || '') + '）；必须使用 HTTPS/反代与强口令';
      } else {
        st.textContent = '当前仅回环监听。开启远程访问需要勾选风险确认并输入 ENABLE-REMOTE。';
      }
    }
    var auth = UI.panel.auth || {};
    var foot = $('#auth-foot');
    if (foot && auth.password_storage) foot.textContent = auth.password_storage + '；Cookie ' + (auth.cookie || 'HttpOnly') + '。';
    renderTls();
  }

  /* ---------------- 内置 TLS ---------------- */
  function renderTls() {
    var t = (UI.panel && UI.panel.tls) || {};
    if ($('#tls-enabled')) $('#tls-enabled').value = t.configured ? 'on' : 'off';
    if ($('#plain-policy')) $('#plain-policy').value = t.plain_policy || 'warn';
    if ($('#tls-cert')) $('#tls-cert').value = t.cert_path || '';
    if ($('#tls-key')) $('#tls-key').value = t.key_path || '';
    var info = $('#tls-info');
    var cert = t.cert;
    if (info) {
      if (t.active && cert) {
        info.textContent = 'HTTPS 生效 · SHA256 ' + cert.fingerprint + ' · 到期 ' + cert.not_after +
          '（剩余 ' + cert.days_left + ' 天）· SAN ' + (cert.sans || []).join(', ');
      } else if (t.configured) {
        info.textContent = '已配置 TLS 但当前进程仍以明文运行，重启后生效。';
      } else {
        info.textContent = '当前明文 HTTP，策略 ' + (t.plain_policy || 'warn') + '；登录凭据未加密传输。';
      }
    }
    var banner = document.getElementById('tls-banner');
    if (!t.active) {
      if (!banner) {
        banner = document.createElement('div');
        banner.id = 'tls-banner';
        banner.style.cssText = 'position:sticky;top:0;z-index:60;padding:8px 14px;font-size:12.5px;' +
          'background:#fff3cd;color:#7a4b00;border-bottom:1px solid #e6c66a;';
        document.body.insertBefore(banner, document.body.firstChild);
      }
      banner.textContent = '⚠ 当前通过明文 HTTP 提供服务（plain_policy=' + (t.plain_policy || 'warn') +
        '），登录凭据未加密。建议在「设置 → 访问与 TLS」启用内置 TLS。';
      banner.style.display = '';
    } else if (banner) {
      banner.style.display = 'none';
    }
  }
  function generateTlsCert() {
    api('/api/tls/generate', { method: 'POST', body: '{}' }).then(function (r) {
      toast('已生成自签证书', '指纹 ' + r.cert.fingerprint + '，到期 ' + r.cert.not_after + '；启用 TLS 并重启后生效');
      loadAll();
    }).catch(function (e) {
      var extra = (e.fields || []).map(function (f) { return f.field + '：' + f.message; }).join(' · ');
      toast('生成失败', e.message + (extra ? ' · ' + extra : ''));
    });
  }
  /* ---------------- 软件更新 ---------------- */
  function fmtPct(n) {
    n = Number(n) || 0;
    return (Math.round(n * 10) / 10) + '%';
  }
  function setVisible(sel, show) {
    var el = $(sel);
    if (el) el.style.display = show ? '' : 'none';
  }
  function setDisabled(sel, disabled) {
    var el = $(sel);
    if (el) el.disabled = !!disabled;
  }

  function renderUpdate() {
    if (!$('#update-card')) return;
    var s = UI.update.status || {};
    var phase = s.phase || 'idle';
    var busy = !!s.busy || ['checking', 'downloading', 'verifying', 'installing'].indexOf(phase) >= 0;
    var current = s.current_version || (UI.panel && UI.panel.version) || '—';
    $('#update-current-version').textContent = current;
    $('#update-current-badge').textContent = 'v' + current;

    var sel = $('#update-mirror');
    if (sel) {
      var mirrors = s.mirrors || [];
      var optionHtml = '<option value="auto">自动：官方 → 镜像</option>' + mirrors.map(function (m) {
        return '<option value="' + esc(m.id) + '">优先镜像 ' + esc(m.id) + '</option>';
      }).join('');
      if (sel.dataset.options !== optionHtml) {
        sel.innerHTML = optionHtml;
        sel.dataset.options = optionHtml;
      }
      var want = s.mirror || 'auto';
      if (sel.value !== want) sel.value = want;
      sel.disabled = busy;
    }

    var available = s.available === true;
    setVisible('#update-meta', available);
    if (available) {
      $('#update-new-version').textContent = 'v' + (s.version || '—');
      $('#update-size').textContent = s.asset_size ? fmtBytes(s.asset_size) : '大小未知';
      $('#update-source').textContent = s.source || '—';
    }

    var saved = s.saved || null;
    var st = $('#update-status');
    var p = s.progress || {};
    var text;
    st.className = 'update-state';
    if (phase === 'checking') {
      text = '正在检查更新…' + (p.attempt ? '（第 ' + p.attempt + ' 个源：' + (p.source || '') + '）' : '');
    } else if (phase === 'downloading') {
      text = '正在下载 v' + (s.version || '') +
        (p.total ? ' · ' + fmtPct(p.percent) + '（' + fmtBytes(p.received) + ' / ' + fmtBytes(p.total) + '）' : '') +
        (p.source ? ' · 来源 ' + p.source : '');
    } else if (phase === 'verifying') {
      text = '下载完成，正在校验签名…';
    } else if (phase === 'installing') {
      text = '正在替换程序并自动重启…';
    } else if (phase === 'ready') {
      text = '已下载并校验通过：' + ((saved && saved.path) || '下载目录');
      st.className = 'update-state good';
    } else if (phase === 'error') {
      text = s.error || '更新失败';
      st.className = 'update-state bad';
    } else if (phase === 'latest') {
      text = '当前已是最新版本（v' + (s.version || current) + '）。';
      st.className = 'update-state good';
    } else if (phase === 'available') {
      text = '发现新版本 v' + (s.version || '') + '，可更新或仅下载。';
    } else {
      text = s.checked === false ? '尚未检查更新。' : '尚未检查更新。';
    }
    if (saved && phase !== 'ready' && phase !== 'error') {
      text += ' 已下载 v' + saved.version + '（校验通过），可安装。';
    }
    st.textContent = text;

    var showProgress = busy && ['checking', 'downloading', 'verifying', 'installing'].indexOf(phase) >= 0 &&
      !(phase === 'installing' && UI.update.waiting);
    setVisible('#update-progress', showProgress);
    if (showProgress) {
      var bar = $('#update-progress-bar');
      var fill = $('#update-progress-fill');
      if (phase === 'downloading' && p.total > 0) {
        bar.classList.remove('indeterminate');
        fill.style.width = fmtPct(p.percent);
      } else {
        bar.classList.add('indeterminate');
        fill.style.width = '';
      }
      $('#update-progress-text').textContent = phase === 'checking' ? '逐个尝试更新源，读取清单'
        : phase === 'verifying' ? '校验清单签名与文件完整性'
        : phase === 'installing' ? '替换程序并重启'
        : ((p.source || '') + ' · ' + fmtBytes(p.received) + (p.total ? ' / ' + fmtBytes(p.total) : '') + (p.total ? ' · ' + fmtPct(p.percent) : ''));
    }

    var fails = (s.attempts || []).filter(function (a) { return !a.ok; });
    var box = $('#update-errors');
    if (fails.length) {
      box.style.display = '';
      box.innerHTML = fails.map(function (a) {
        return '<div class="update-error-row"><b>' + esc(a.source) + '</b><span>' + esc(a.error) + '</span></div>';
      }).join('');
    } else {
      box.style.display = 'none';
      box.innerHTML = '';
    }

    var savedSame = !!(saved && available && saved.version === s.version);
    setVisible('#update-update', available && !busy);
    setVisible('#update-save', available && !busy && !savedSame);
    setVisible('#update-install', !!saved && !busy && (!available || savedSame));
    setVisible('#update-retry', phase === 'error' && !busy);
    setDisabled('#update-check', busy);
    setDisabled('#update-update', busy);
    setDisabled('#update-save', busy);
    setDisabled('#update-install', busy);
    $('#update-check').textContent = phase === 'checking' ? '检查中…' : '检查更新';
  }

  function pollUpdate(done) {
    stopUpdatePoll();
    function fetchStatus() {
      return api('/api/update/status').then(function (s) {
        UI.update.status = s;
        renderUpdate();
        if (s.phase === 'installing' && !s.busy) {
          stopUpdatePoll();
          return;
        }
        if (s.phase === 'installing' && !UI.update.waiting) {
          stopUpdatePoll();
          UI.update.waiting = true;
          waitForRestart();
          return;
        }
        if (!s.busy) {
          stopUpdatePoll();
          if (done) done(s);
        }
      }).catch(function () { /* 更新重启期间连接会中断，由 waitForRestart 兜底 */ });
    }
    UI.update.poll = setInterval(fetchStatus, 600);
    return fetchStatus();
  }

  function stopUpdatePoll() {
    if (UI.update.poll) {
      clearInterval(UI.update.poll);
      UI.update.poll = null;
    }
  }

  function waitForRestart() {
    var tries = 0;
    setTimeout(function () {
      var timer = setInterval(function () {
        tries++;
        fetch('/healthz', { cache: 'no-store' }).then(function (r) {
          if (r.ok) {
            clearInterval(timer);
            UI.update.waiting = false;
            location.reload();
          }
        }).catch(function () {});
        if (tries >= 90) {
          clearInterval(timer);
          UI.update.waiting = false;
          toast('更新已提交', '若页面未自动恢复，请手动刷新');
        }
      }, 1000);
    }, 2500);
  }

  function startUpdateCheck() {
    UI.update.lastAction = 'check';
    setDisabled('#update-check', true);
    $('#update-check').textContent = '检查中…';
    api('/api/update/check', { method: 'POST', body: JSON.stringify({}) })
      .then(function () { pollUpdate(); })
      .catch(function (e) { toast('检查更新失败', e.message); pollUpdate(); });
  }

  function startUpdateDownload(save) {
    UI.update.lastAction = save ? 'save' : 'install';
    api('/api/update/download', { method: 'POST', body: JSON.stringify({ save: !!save }) })
      .then(function () { pollUpdate(); })
      .catch(function (e) { toast('下载失败', e.message); pollUpdate(); });
  }

  function startSavedInstall() {
    var saved = (UI.update.status || {}).saved;
    if (!saved) return;
    UI.update.lastAction = 'install';
    api('/api/update/install', { method: 'POST', body: JSON.stringify({ path: saved.path }) })
      .then(function () { pollUpdate(); })
      .catch(function (e) { toast('安装失败', e.message); });
  }

  function retryUpdate() {
    if (UI.update.lastAction === 'save' || UI.update.lastAction === 'install') {
      startUpdateDownload(UI.update.lastAction === 'save');
    } else {
      startUpdateCheck();
    }
  }

  function bindUpdateEvents() {
    var check = $('#update-check');
    if (check) check.addEventListener('click', startUpdateCheck);
    var update = $('#update-update');
    if (update) update.addEventListener('click', function () { startUpdateDownload(false); });
    var save = $('#update-save');
    if (save) save.addEventListener('click', function () { startUpdateDownload(true); });
    var install = $('#update-install');
    if (install) install.addEventListener('click', startSavedInstall);
    var retry = $('#update-retry');
    if (retry) retry.addEventListener('click', retryUpdate);
    var mirror = $('#update-mirror');
    if (mirror) mirror.addEventListener('change', function () {
      api('/api/panel', { method: 'PATCH', body: JSON.stringify({ update_mirror: mirror.value }) })
        .then(function () {
          toast('更新源已切换', mirror.value === 'auto' ? '官方 → 镜像' : '优先镜像 ' + mirror.value);
          api('/api/update/status').then(function (s) { UI.update.status = s; renderUpdate(); });
        })
        .catch(function (e) { toast('切换失败', e.message); });
    });
  }

  function stateListenIsLoopback(host) {
    return host === '127.0.0.1' || host === 'localhost' || host === '::1' || host === '[::1]' || /^127\./.test(host);
  }
  function savePanelConfig() {
    var listenHost = ($('#panel-listen') || {}).value || '127.0.0.1';
    var port = ($('#panel-port') || {}).value || '18080';
    var body = {
      panel_address: ($('#panel-address-input') || {}).value.trim(),
      listen: listenHost + ':' + port,
      trusted_proxies: String(($('#trusted-proxies') || {}).value || '').split(',').map(function (s) { return s.trim(); }).filter(Boolean),
      tls_enabled: (($('#tls-enabled') || {}).value) === 'on',
      tls_cert: (($('#tls-cert') || {}).value || '').trim(),
      tls_key: (($('#tls-key') || {}).value || '').trim(),
      plain_policy: (($('#plain-policy') || {}).value) || 'warn'
    };
    var wantRemote = !stateListenIsLoopback(listenHost);
    if (wantRemote) {
      if (!($('#remote-ack') || {}).checked) {
        toast('未保存', '开启远程访问前必须勾选「已阅读并确认风险」');
        return;
      }
      body.remote_enabled = true;
      body.confirm_remote = ($('#remote-confirm') || {}).value.trim();
    } else {
      body.remote_enabled = false;
    }
    api('/api/panel', { method: 'PATCH', body: JSON.stringify(body) }).then(function (r) {
      UI.panel.panel_address = r.panel_address;
      UI.panel.listen = r.listen;
      UI.panel.listen_effective = r.listen_effective;
      UI.panel.remote_access = Object.assign({}, UI.panel.remote_access || {}, r.remote_access);
      if (r.tls) UI.panel.tls = Object.assign({}, UI.panel.tls || {}, r.tls, { cert: (UI.panel.tls || {}).cert });
      renderSettings();
      toast('设置已保存', r.tls && r.tls.restart_required ? 'TLS/监听变更需重启面板生效' : (r.note || '重启面板后监听变更生效'));
      schedulePreview();
    }).catch(function (e) {
      var extra = (e.fields || []).map(function (f) { return f.field + '：' + f.message; }).join(' · ');
      toast('保存失败', e.message + (extra ? ' · ' + extra : ''));
    });
  }
  function disableRemote() {
    api('/api/panel', { method: 'PATCH', body: JSON.stringify({ remote_enabled: false, listen: '127.0.0.1:' + ((($('#panel-port') || {}).value) || '18080') }) }).then(function () {
      toast('已关闭远程访问', '监听将在重启后回到 127.0.0.1');
      if ($('#remote-ack')) $('#remote-ack').checked = false;
      if ($('#remote-confirm')) $('#remote-confirm').value = '';
      loadAll();
    }).catch(function (e) { toast('操作失败', e.message); });
  }
  function changePassword() {
    var body = {
      current: ($('#pwd-current') || {}).value || '',
      new: ($('#pwd-new') || {}).value || '',
      confirm: ($('#pwd-confirm') || {}).value || ''
    };
    api('/api/auth/password', { method: 'POST', body: JSON.stringify(body) }).then(function () {
      AUTH.csrf = '';
      toast('口令已更新', '所有会话已失效，请重新登录');
      ['#pwd-current', '#pwd-new', '#pwd-confirm'].forEach(function (s) { if ($(s)) $(s).value = ''; });
      showAuth('login');
    }).catch(function (e) {
      var extra = (e.fields || []).map(function (f) { return f.field + '：' + f.message; }).join(' · ');
      toast('更新失败', e.message + (extra ? ' · ' + extra : ''));
    });
  }

  /* ---------------- 进程管理（M3） ---------------- */
  var PROC_BADGE = {
    running: ['ok', '运行中'],
    starting: ['info', '启动中'],
    stopping: ['muted', '停止中'],
    stopped: ['muted', '已停止'],
    failed: ['warn', '失败'],
    config_invalid: ['warn', '配置校验失败'],
    auto_restarting: ['info', '自动拉起中'],
    demo: ['muted', '演示模式']
  };
  function renderProcesses() {
    var box = $('#proc-grid');
    if (!box || !UI.kernels) return;
    var runtime = UI.kernels.runtime || {};
    var demo = !!UI.kernels.demo;
    var badge = $('#proc-demo-badge');
    if (badge) {
      badge.className = 'badge ' + (demo ? 'muted' : 'info') + ' plain';
      badge.textContent = demo ? '演示模式 · 不启动真实内核' : '真实进程管理';
    }
    box.innerHTML = ['xray', 'sing-box', 'mihomo'].map(function (k) {
      var r = runtime[k] || { status: 'stopped' };
      var st = PROC_BADGE[r.status] || ['muted', r.status || '未知'];
      var bin = r.binary || {};
      var sha = r.config_sha256 ? r.config_sha256.slice(0, 12) + '…' : '—';
      var portInfo = r.pid ? ('pid ' + r.pid + ' · 运行 ' + (r.uptime_secs || 0) + 's') : '无进程';
      var alert = r.alert || r.last_error || '';
      return '<div class="proc-row">' +
        '<div class="proc-top"><b>' + esc(k) + '</b><span class="badge ' + st[0] + ' plain">' + esc(st[1]) + '</span>' +
        '<span class="badge ' + (bin.present ? 'ok' : 'muted') + ' plain">' + (bin.present ? esc(bin.version || '已安装') : '未安装') + '</span>' +
        '<span class="badge muted plain">拉起 ' + (r.restarts || 0) + '/' + (r.restart_limit || 3) + '</span>' +
        '<span class="badge ' + (r.adopted ? 'info' : 'muted') + ' plain">' + (r.adopted ? '面板重启后接管' : '') + '</span></div>' +
        '<div class="proc-meta">' + esc(portInfo) + ' · config sha ' + esc(sha) + ' · ' + esc(bin.path || '—') + '</div>' +
        (alert ? '<div class="proc-alert">' + esc(alert) + '</div>' : '') +
        '<div class="proc-actions">' +
        '<button class="btn btn-sm" data-proc="start" data-kernel="' + esc(k) + '">启动</button>' +
        '<button class="btn btn-soft btn-sm" data-proc="stop" data-kernel="' + esc(k) + '">停止</button>' +
        '<button class="btn btn-soft btn-sm" data-proc="restart" data-kernel="' + esc(k) + '">重启</button>' +
        '<button class="btn btn-soft btn-sm" data-proc="validate" data-kernel="' + esc(k) + '">校验配置</button>' +
        '<button class="btn btn-soft btn-sm" data-proc="log" data-kernel="' + esc(k) + '">日志</button>' +
        '</div></div>';
    }).join('');
    var note = $('#proc-note');
    if (note) {
      note.textContent = demo
        ? '演示模式：不会 spawn 任何真实内核，启停按钮只返回 409 demo_mode。'
        : 'state/kernels.json 记录 PID/状态；面板重启核对归属，孤儿清理；异常退出自动拉起（上限 3 次），失败回滚 last-known-good。';
    }
  }
  function bindProcessEvents() {
    var box = $('#proc-grid');
    if (!box) return;
    box.addEventListener('click', function (ev) {
      var b = ev.target.closest('[data-proc]');
      if (!b) return;
      var k = b.dataset.kernel;
      var act = b.dataset.proc;
      if (DEMO) { toast('演示模式', '不会启动真实内核，也不写系统配置'); return; }
      if (act === 'log') {
        api('/api/kernels/' + k + '/log?tail=120').then(function (r) {
          var lines = (r.lines || []).slice(-40).join('\n');
          toast(k + ' 日志（末 40 行）', lines || '（日志为空）');
        }).catch(function (e) { toast('读取日志失败', e.message); });
        return;
      }
      b.disabled = true;
      api('/api/kernels/' + k + '/' + act, { method: 'POST' }).then(function (r) {
        if (act === 'validate') {
          toast(k + ' 配置校验', (r.ok ? '通过' : '失败') + ' · ' + (r.output || ''));
        } else if (act === 'start' || act === 'restart') {
          toast(k + ' 已启动', 'pid ' + (r.pid || '?') + ' · 配置自检通过');
        } else {
          toast(k + ' 已停止', 'pid ' + (r.pid || '?'));
        }
        return refreshKernels();
      }).catch(function (e) {
        var fields = e.fields || [];
        var extra = fields.map(function (f) {
          if (f.field === 'owner') {
            try {
              var o = JSON.parse(f.message);
              return '占用者 PID ' + (o.pid || '?') + ' · ' + (o.name || '未知进程');
            } catch (x) { return f.message; }
          }
          return f.field + '：' + f.message;
        }).join(' · ');
        toast(k + ' ' + act + ' 失败', e.message + (extra ? ' · ' + extra : ''));
        refreshKernels();
      }).then(function () { b.disabled = false; });
    });
  }
  function refreshKernels() {
    return api('/api/kernels').then(function (r) {
      UI.kernels = r;
      renderKernels();
      renderProcesses();
      renderChrome();
    });
  }

  /* ---------------- 鉴权（M3） ---------------- */
  function showAuth(mode) {
    var ov = $('#auth-overlay');
    if (!ov) return;
    AUTH.mode = mode;
    ov.classList.add('open');
    $('#auth-title').textContent = mode === 'setup' ? '首次启动 · 设置管理员口令' : '管理员登录';
    $('#auth-sub').textContent = mode === 'setup'
      ? '这是第一次启动。口令只保存哈希，不落明文；请使用至少 10 位、包含字母与数字的口令。'
      : '面板默认只监听 127.0.0.1。所有写操作需要会话 + CSRF。';
    $('#auth-confirm-field').hidden = mode !== 'setup';
    $('#auth-submit').textContent = mode === 'setup' ? '设置口令并登录' : '登录';
    $('#auth-msg').textContent = '';
    $('#auth-msg').className = 'auth-msg';
    setTimeout(function () { $('#auth-password').focus(); }, 50);
  }
  function hideAuth() {
    var ov = $('#auth-overlay');
    if (ov) ov.classList.remove('open');
  }
  function authReason(e) {
    var msg = e.message || '请求失败';
    if (e.status === 429) msg += '（已触发速率限制，请稍候再试）';
    var fields = (e.fields || []).map(function (f) { return f.field + '：' + f.message; });
    return msg + (fields.length ? ' · ' + fields.join(' · ') : '');
  }
  function authBoot() {
    api('/api/auth/status').then(function (st) {
      AUTH.status = st;
      if (!st.initialized) { showAuth('setup'); return; }
      if (!st.authenticated) { showAuth('login'); return; }
      AUTH.csrf = st.csrf_token || '';
      hideAuth();
      loadAll();
    }).catch(function (e) {
      toast('无法获取鉴权状态', e.message);
      showAuth('login');
    });
  }
  function bindAuthEvents() {
    var form = $('#auth-form');
    if (!form) return;
    form.addEventListener('submit', function (ev) {
      ev.preventDefault();
      var mode = AUTH.mode || 'login';
      var password = ($('#auth-password') || {}).value || '';
      var confirm = ($('#auth-confirm') || {}).value || '';
      var msg = $('#auth-msg');
      var submit = $('#auth-submit');
      submit.disabled = true;
      var path = mode === 'setup' ? '/api/auth/setup' : '/api/auth/login';
      var body = mode === 'setup' ? { password: password, confirm: confirm } : { password: password };
      fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, credentials: 'same-origin', body: JSON.stringify(body) })
        .then(function (res) {
          return res.text().then(function (text) {
            var data = {};
            try { data = JSON.parse(text); } catch (e) {}
            if (!res.ok) {
              var err = new Error(data.error && data.error.message || ('HTTP ' + res.status));
              err.status = res.status;
              err.fields = data.error && data.error.fields || [];
              throw err;
            }
            return data;
          });
        })
        .then(function () {
          if (mode === 'setup') {
            return fetch('/api/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, credentials: 'same-origin', body: JSON.stringify({ password: password }) })
              .then(function (res) { return res.json(); });
          }
          return null;
        })
        .then(function (login) {
          if (login && login.csrf_token) AUTH.csrf = login.csrf_token;
          msg.className = 'auth-msg auth-ok';
          msg.textContent = mode === 'setup' ? '口令已设置，正在进入面板…' : '登录成功';
          submit.disabled = false;
          $('#auth-password').value = '';
          if ($('#auth-confirm')) $('#auth-confirm').value = '';
          api('/api/auth/status').then(function (st) {
            AUTH.status = st;
            AUTH.csrf = st.csrf_token || AUTH.csrf;
            hideAuth();
            loadAll();
          });
        })
        .catch(function (e) {
          submit.disabled = false;
          msg.className = 'auth-msg';
          msg.textContent = authReason(e);
        });
    });
    $('#pwd-update').addEventListener('click', changePassword);
    $('#remote-disable').addEventListener('click', disableRemote);
  }

  document.addEventListener('DOMContentLoaded', function () {
    if (!$('#wizard-overlay')) return;
    ensureHintBoxForInit();
    boot();
  });

  function ensureHintBoxForInit() {
    wizardStepHint = document.createElement('div');
    wizardStepHint.className = 'wz-hint';
    var err = $('#wz-error');
    err.parentNode.insertBefore(wizardStepHint, err);
  }
})();
