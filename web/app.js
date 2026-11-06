/* Berthio 面板前端（M1）
 * 单一来源：与设计稿同一套 DOM / 组件；本文件只负责数据接入与交互。
 * 静态 demo 模式通过 window.__BERTHIO_DEMO_DATA__ 注入同一接口形状的假数据，
 * 不另写一套 UI。 */
(function () {
  'use strict';

  var DEMO = !!window.__BERTHIO_DEMO_DATA__;
  var FIX = window.__BERTHIO_DEMO_DATA__ || null;

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
    export: {
      format: 'clash', scenario: 'desktop', bucket: '112',
      rules: {}, order: [], preset: 'daily', nodeSel: {}, preview: null, ack: false, timer: null
    }
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
    return { ok: true };
  }

  function api(path, opts) {
    if (DEMO) {
      if (opts && opts.method === 'POST') return Promise.resolve(demoPost(path.split('?')[0], JSON.parse(opts.body || '{}')));
      if (path.indexOf('/api/export/preview') === 0) {
        return Promise.resolve(clone(FIX.exportPreview));
      }
      var key = path.split('?')[0];
      if (key === '/api/logs') return Promise.resolve(clone(FIX.logs));
      if (key === '/api/rulesets') return Promise.resolve(clone(FIX.rulesets));
      if (key === '/api/kernels') return Promise.resolve(clone(FIX.kernels));
      if (key === '/api/inbound/inbounds') return Promise.resolve({ items: clone(UI.inbounds), total: UI.inbounds.length, kernels: { xray: UI.inbounds.length }, selection: {} });
      if (key === '/api/inbound/wizard/defaults') return Promise.resolve(clone(FIX.defaults));
      if (key === '/api/panel') return Promise.resolve(clone(FIX.panel));
      if (key.indexOf('/api/inbound/inbounds/') === 0 && key.slice(-8) === '/clients') {
        var iid = decodeURIComponent(key.slice('/api/inbound/inbounds/'.length, key.length - '/clients'.length));
        return Promise.resolve({ inbound_id: iid, items: clone((FIX.clients || {})[iid] || []) });
      }
      return Promise.resolve({});
    }
    return fetch(path, Object.assign({ headers: { 'Content-Type': 'application/json' } }, opts || {}))
      .then(function (res) {
        return res.text().then(function (text) {
          var body = text;
          try { body = JSON.parse(text); } catch (e) {}
          if (!res.ok) {
            var msg = (body && body.error && body.error.message) || ('HTTP ' + res.status);
            var err = new Error(msg);
            err.status = res.status;
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
    $('#panel-save').addEventListener('click', savePanelAddress);
    $('#wz-protocol').addEventListener('change', updateWizardFields);
    $('#wz-port').addEventListener('input', updateWizardFields);

    bindExportEvents();
    loadAll();
  }

  function loadAll() {
    return Promise.all([
      api('/api/panel'), api('/api/inbound/inbounds'), api('/api/rulesets'), api('/api/kernels'), api('/api/logs'),
      api('/api/inbound/wizard/defaults')
    ]).then(function (r) {
      UI.panel = r[0];
      UI.inbounds = r[1].items || [];
      UI.rules = r[2];
      UI.kernels = r[3];
      UI.logs = r[4];
      UI.defaults = r[5];
      if ((UI.panel && UI.panel.demo) || DEMO) $('#demo-banner').classList.add('show');
      renderChrome();
      renderOverview();
      renderInbounds();
      renderRulesets();
      renderNodes();
      renderKernels();
      renderLogs();
      renderSettings();
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
      body.innerHTML = '<div class="onboard-head"><div class="onboard-line"><b>M1 已接通真实接口</b><span>入站列表 / 新建向导 / 导出 / 内核探测</span></div><span class="badge info plain">' + esc(p.version || '') + '</span></div>' +
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
      statCard('规则集', total + ' <small>条</small>', '官方源 · 镜像选源在 M2', 'accent');
  }

  function statCard(label, value, foot, kind) {
    return '<div class="stat"><div class="stat-top"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"></circle><path d="M12 3v3M12 18v3M3 12h3M18 12h3"></path></svg>' +
      label + '<span class="badge ' + kind + ' plain">M1</span></div><div class="stat-value">' + value + '</div>' +
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
        : 'TLS：M1 只记录自签证书路径占位，不生成证书、不写系统目录。';
    }
    if (step === 5) return '防火墙：M1 不写宿主规则，只在事务摘要里标 skipped；需要开放端口请自行在云安全组处理。';
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
        html += '<div class="rs-row" data-rule-id="' + esc(r.id) + '">' +
          '<span class="rs-idx">' + (on ? idx : '—') + '</span>' +
          '<div class="rs-main"><b>' + esc(r.display) + '</b><span class="rs-id">' + esc(r.real_id) + '</span><span class="rs-meta">' + fmtBytes(size) + '</span><span class="rs-src">← 官方源</span></div>' +
          '<select class="select rs-act" data-rule="' + esc(r.id) + '">' + acts.map(function (a) {
            return '<option value="' + a.id + '"' + ((UI.export.rules[r.id] || 'off') === a.id ? ' selected' : '') + '>' + esc(a.label) + '</option>';
          }).join('') + '</select>' +
          '<details class="rs-detail"><summary>详情</summary><div class="rs-detail-body"><div>官方：<span class="mono">https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat</span></div><div>镜像：M2 接入（SHA-256 以官方为准）</div></div></details>' +
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
        $('#export-tokens').innerHTML = '<div class="kv"><span class="k">token</span><span class="v mono">' + esc(r.token) + '</span></div><div class="table-note">' + esc(r.note || '只展示一次；服务端仅存 SHA-256') + '</div>';
        toast('订阅令牌已生成', r.url);
      }).catch(function (e) { toast('签发失败', e.message); });
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
  }
  function savePanelAddress() {
    var v = $('#panel-address-input').value.trim();
    api('/api/panel', { method: 'PATCH', body: JSON.stringify({ panel_address: v }) }).then(function (r) {
      UI.panel.panel_address = r.panel_address;
      toast('已保存对外地址', r.panel_address);
      schedulePreview();
    }).catch(function (e) {
      var extra = (e.fields || []).map(function (f) { return f.field + '：' + f.message; }).join('\n');
      toast('保存失败', e.message + (extra ? ' · ' + extra : ''));
    });
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
