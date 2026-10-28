<script lang="ts">
	import { onMount } from 'svelte';
	import ConsoleBar from '#lib/components/ConsoleBar.svelte';
	import { consoleText, fill } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const c = $derived(consoleText[i18n.lang]);
	const t = $derived(c.admin);

	const TABS = ['overview', 'alerts', 'accounts', 'apps', 'audit'] as const;
	type Tab = (typeof TABS)[number];
	let tab: Tab = $state('overview');

	// The tab lives in the URL's fragment, so a form's answer (whose action
	// carries it) and a reload come back to it.
	onMount(() => {
		const from = location.hash.slice(1) as Tab;
		if (TABS.includes(from)) tab = from;
	});
	function show(next: Tab) {
		tab = next;
		history.replaceState(history.state, '', `#${next}`);
	}

	// Formatting.
	function bytes(n: number): string {
		if (n < 1e3) return `${n} B`;
		const [unit, size] = n < 1e6 ? ['KB', 1e3] : n < 1e9 ? ['MB', 1e6] : ['GB', 1e9];
		return `${(n / size).toFixed(n / size < 10 ? 1 : 0)} ${unit}`;
	}
	const usd = (n: number) => (n === 0 ? '$0' : n < 0.01 ? '< $0.01' : `$${n.toFixed(2)}`);
	const count = (n: number) => n.toLocaleString(i18n.lang === 'zh' ? 'zh-CN' : 'en-US');
	const stamp = (ms: number) => new Date(ms).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
	function ago(ms: number): string {
		const minutes = Math.floor((Date.now() - ms) / 60_000);
		if (minutes < 1) return t.ago.now;
		if (minutes < 60) return fill(t.ago.m, { n: minutes });
		if (minutes < 48 * 60) return fill(t.ago.h, { n: Math.floor(minutes / 60) });
		return fill(t.ago.d, { n: Math.floor(minutes / 1440) });
	}
	const short = (id: string) => (id.length > 12 ? `${id.slice(0, 10)}…` : id);
	const confirmed = (text: string) => (e: SubmitEvent) => {
		if (!confirm(text)) e.preventDefault();
	};

	// Search, for accounts and apps alike.
	let query = $state('');
	const matches = (...fields: string[]) =>
		!query.trim() || fields.some((f) => f.toLowerCase().includes(query.trim().toLowerCase()));
	const accounts = $derived(data.accounts.filter((a) => matches(a.login, a.id)));
	const apps = $derived(data.apps.filter((p) => matches(p.name, p.id, p.login)));

	// The audit log: one source at a time, filtered, 50 rows at first.
	let source: 'site' | 'relay' = $state('site');
	let action = $state('');
	let auditQuery = $state('');
	let shown = $state(30);
	/** An action or an actor, in words; the code stays in the tooltip. */
	const said = (action: string) => t.actions[action] ?? action;
	const by = (actor: { kind: string; name: string }) =>
		actor.kind ? fill(t.actors[actor.kind] ?? actor.kind, { name: actor.name }) : actor.name;
	const rows = $derived(
		source === 'site'
			? data.audit.map((r) => ({
					at: r.at,
					action: r.action,
					id: '',
					subject: r.who ? `@${r.who}` : '',
					detail: r.detail,
					actor: by(r.actor)
				}))
			: data.issuance.map((r) => ({
					at: r.at,
					action: r.action,
					id: r.client,
					subject: r.client.startsWith('t-') ? t.trialClient : short(r.client),
					detail: r.gen === null ? '' : fill(t.generation, { n: r.gen }),
					actor: by(r.actor)
				}))
	);
	const actions = $derived([...new Set(rows.map((r) => r.action))].sort());
	const filtered = $derived(
		rows.filter(
			(r) =>
				(!action || r.action === action) &&
				(!auditQuery.trim() ||
					[said(r.action), r.subject, r.detail, r.actor]
						.join(' ')
						.toLowerCase()
						.includes(auditQuery.trim().toLowerCase()))
		)
	);

	// The daily chart: one series, so no legend; the title names it.
	// Drawn at the width it is shown at, so its text stays 11px on any screen.
	let measured = $state(0);
	const W = $derived(measured > 0 ? measured : 720);
	const H = 180;
	const PAD = { top: 12, right: 8, bottom: 26, left: 44 };
	const top = $derived.by(() => {
		const max = Math.max(1, ...data.days.map((d) => d.connections));
		const step = 10 ** Math.floor(Math.log10(max));
		return Math.ceil(max / step) * step;
	});
	const slot = $derived((W - PAD.left - PAD.right) / Math.max(data.days.length, 1));
	const barWidth = $derived(Math.max(2, Math.min(16, slot * 0.6)));
	const y = (v: number) => PAD.top + (H - PAD.top - PAD.bottom) * (1 - v / top);
	let hovered: number | null = $state(null);
	const hover = $derived(hovered === null ? null : data.days[hovered]);

	const kinds = $derived(t.kinds as Record<string, string>);

	/** An audit action's tone: what took access away, what gave it, or neither. */
	function tone(action: string): 'bad' | 'good' | '' {
		if (/delete|suspend|disable|revoke|refuse|rotate/.test(action)) return 'bad';
		if (/create|approve|exchange|mint|enable|reactivate|bound/.test(action)) return 'good';
		return '';
	}
</script>

<svelte:head>
	<title>{c.title.admin} · PeerConnect</title>
	<meta name="robots" content="noindex" />
</svelte:head>

{#snippet who(login: string, id: string, avatar = '')}
	<span class="who">
		{#if avatar}<img src={avatar} alt="" width="28" height="28" />{:else}<span
				class="dot"
				aria-hidden="true"
			></span>{/if}
		<span><strong>@{login}</strong><br /><code title={id}>{short(id)}</code></span>
	</span>
{/snippet}

{#snippet when(ms: number)}
	<time datetime={new Date(ms).toISOString()} title={stamp(ms)}>{ago(ms)}</time>
{/snippet}

{#snippet alertRows(list: typeof data.alerts)}
	<ul class="alerts">
		{#each list as a (a.day + a.client + a.kind)}
			<li data-alert={a.kind}>
				<span class="tag warn"><span aria-hidden="true">⚠</span> {kinds[a.kind] ?? a.kind}</span>
				<span class="alert-who"
					>{#if a.login}<strong>@{a.login}</strong>{:else}<span class="muted"
							>{t.deletedClient}</span
						>{/if} <code title={a.client}>{short(a.client)}</code></span
				>
				<span class="num"
					>{a.kind === 'bytes' ? bytes(a.value) : count(a.value)}
					<span class="muted">≥ {a.kind === 'bytes' ? bytes(a.threshold) : count(a.threshold)}</span
					></span
				>
				<span class="muted">{@render when(a.at)}</span>
			</li>
		{/each}
	</ul>
{/snippet}

<section class="container console">
	<ConsoleBar login={data.login} avatar={data.avatar} csrf={data.csrf} admin />
	<h1>{c.title.admin}</h1>

	{#if form?.done}<p class="note" role="status">{fill(t.done, { id: form.done })}</p>{/if}
	{#if form?.more}
		<form
			class="note warn-note"
			method="POST"
			action={form.more.kind === 'app' ? '?/app#apps' : '?/account#accounts'}
		>
			<span>{t.more}</span>
			<input type="hidden" name="csrf" value={data.csrf} />
			<input type="hidden" name={form.more.kind} value={form.more.id} />
			<input
				type="hidden"
				name="state"
				value={form.more.kind === 'app' ? 'disabled' : 'suspended'}
			/>
			<input type="hidden" name="offset" value={form.more.offset} />
			<button class="button danger small" type="submit">{t.goOn}</button>
		</form>
	{/if}

	<div class="kpis">
		<div class="kpi">
			<span class="label">{t.kpi.accounts}</span><span class="value"
				>{count(data.totals.accounts)}</span
			>
			<span class="sub"
				>{data.totals.suspended ? fill(t.kpi.suspended, { n: data.totals.suspended }) : ' '}</span
			>
		</div>
		<div class="kpi">
			<span class="label">{t.kpi.apps}</span><span class="value">{count(data.totals.apps)}</span>
			<span class="sub"
				>{data.totals.disabled ? fill(t.kpi.disabled, { n: data.totals.disabled }) : ' '}</span
			>
		</div>
		<div class="kpi">
			<span class="label">{t.kpi.clients}</span><span class="value"
				>{count(data.totals.clients)}</span
			>
		</div>
		<button class="kpi link" class:hot={data.totals.alertsToday > 0} onclick={() => show('alerts')}>
			<span class="label">{t.kpi.alertsToday}</span><span class="value"
				>{count(data.totals.alertsToday)}</span
			>
		</button>
		<div class="kpi">
			<span class="label">{t.kpi.connections}</span><span class="value"
				>{count(data.totals.connections)}</span
			>
		</div>
		<div class="kpi">
			<span class="label">{t.kpi.traffic}</span><span class="value">{bytes(data.totals.bytes)}</span
			>
		</div>
		<div class="kpi">
			<span class="label">{t.kpi.cost}</span><span class="value">{usd(data.totals.cost)}</span>
		</div>
	</div>

	<div class="tabs" role="tablist" aria-label={c.title.admin}>
		{#each TABS as name (name)}
			<button
				role="tab"
				id="tab-{name}"
				aria-selected={tab === name}
				aria-controls="panel-{name}"
				onclick={() => show(name)}
			>
				{t.tabs[name]}
				{#if name === 'alerts' && data.alerts.length}<span class="badge">{data.alerts.length}</span
					>{/if}
				{#if name === 'accounts'}<span class="badge quiet">{data.accounts.length}</span>{/if}
				{#if name === 'apps'}<span class="badge quiet">{data.apps.length}</span>{/if}
			</button>
		{/each}
	</div>

	<div
		class="panel grid"
		role="tabpanel"
		id="panel-overview"
		aria-labelledby="tab-overview"
		hidden={tab !== 'overview'}
	>
		<section class="card wide">
			<header>
				<h2>{t.chart}</h2>
				<p class="muted small-text">{t.chartNote}</p>
			</header>
			{#if data.days.length === 0}
				<p class="empty">{t.noUsage}</p>
			{:else}
				<div class="chart" bind:clientWidth={measured}>
					<p class="readout" aria-live="polite">
						{#if hover}
							<strong>{hover.day}</strong> · {count(hover.connections)}
							{c.common.connections} · {bytes(hover.bytes)} · {usd(hover.cost)}
						{:else}&nbsp;{/if}
					</p>
					<svg viewBox="0 0 {W} {H}" role="img" aria-label={t.chart}>
						{#each [0, 0.5, 1] as f (f)}
							<line
								class="grid-line"
								x1={PAD.left}
								x2={W - PAD.right}
								y1={y(top * f)}
								y2={y(top * f)}
							/>
							<text class="axis" x={PAD.left - 6} y={y(top * f) + 4} text-anchor="end"
								>{count(top * f)}</text
							>
						{/each}
						{#each data.days as d, i (d.day)}
							{@const x = PAD.left + i * slot + (slot - barWidth) / 2}
							{@const h = Math.max(d.connections > 0 ? 1 : 0, y(0) - y(d.connections))}
							<path
								class="bar"
								class:dim={hovered !== null && hovered !== i}
								d="M{x},{y(0)} v{-Math.max(0, h - 4)} q0,-4 4,-4 h{barWidth -
									8} q4,0 4,4 v{Math.max(0, h - 4)} z"
							/>
							<!-- A hit target the full height of the plot, wider than the bar. -->
							<rect
								role="presentation"
								class="hit"
								x={PAD.left + i * slot}
								y={PAD.top}
								width={slot}
								height={H - PAD.top - PAD.bottom}
								onpointerenter={() => (hovered = i)}
								onpointerleave={() => (hovered = null)}
								><title>{d.day}: {count(d.connections)}</title></rect
							>
						{/each}
						{#each [0, Math.floor((data.days.length - 1) / 2), data.days.length - 1] as i (i)}
							<text
								class="axis"
								x={i === 0
									? PAD.left
									: i === data.days.length - 1
										? W - PAD.right
										: PAD.left + i * slot + slot / 2}
								y={H - 8}
								text-anchor={i === 0 ? 'start' : i === data.days.length - 1 ? 'end' : 'middle'}
								>{data.days[i].day.slice(5)}</text
							>
						{/each}
					</svg>
					<details class="table-view">
						<summary>{t.showTable}</summary>
						<div class="table">
							<table>
								<thead
									><tr
										><th>{t.day}</th><th class="num">{c.common.connections}</th><th class="num"
											>{c.common.traffic}</th
										><th class="num">{t.cost}</th></tr
									></thead
								>
								<tbody>
									{#each [...data.days].reverse() as d (d.day)}
										<tr
											><td>{d.day}</td><td class="num">{count(d.connections)}</td><td class="num"
												>{bytes(d.bytes)}</td
											><td class="num">{usd(d.cost)}</td></tr
										>
									{/each}
								</tbody>
							</table>
						</div>
					</details>
					<p class="muted small-text">{t.costNote}</p>
				</div>
			{/if}
		</section>

		<section class="card">
			<header><h2>{t.top}</h2></header>
			{#if data.top.length === 0}
				<p class="empty">{t.noUsage}</p>
			{:else}
				<ol class="ranking">
					{#each data.top as a (a.id)}
						<li>
							<span><strong>@{a.login}</strong></span>
							<span class="num muted">{count(a.connections)} · {bytes(a.bytes)}</span>
							<span class="num">{usd(a.cost)}</span>
						</li>
					{/each}
				</ol>
			{/if}
		</section>

		<section class="card">
			<header>
				<h2>{t.latestAlerts}</h2>
				{#if data.alerts.length}<button class="linklike" onclick={() => show('alerts')}
						>{t.allAlerts} →</button
					>{/if}
			</header>
			{#if data.alerts.length === 0}
				<p class="empty">{t.noAlerts}</p>
			{:else}
				{@render alertRows(data.alerts.slice(0, 5))}
			{/if}
		</section>
	</div>
	<div
		class="panel"
		role="tabpanel"
		id="panel-alerts"
		aria-labelledby="tab-alerts"
		hidden={tab !== 'alerts'}
	>
		<p class="muted">{t.alertsNote}</p>
		{#if data.alerts.length === 0}
			<p class="empty card">{t.noAlerts}</p>
		{:else}
			<div class="card flush">{@render alertRows(data.alerts)}</div>
		{/if}
	</div>
	<div
		class="panel"
		role="tabpanel"
		id="panel-accounts"
		aria-labelledby="tab-accounts"
		hidden={tab !== 'accounts'}
	>
		<input
			class="search"
			type="search"
			placeholder={t.search}
			bind:value={query}
			aria-label={t.search}
		/>
		{#if accounts.length === 0}
			<p class="empty card">{t.noMatch}</p>
		{:else}
			<div class="table card flush">
				<table>
					<thead>
						<tr>
							<th>{t.account}</th><th>{t.status}</th><th>{t.seen}</th><th class="num">{t.apps}</th
							><th class="num">{t.clients}</th><th class="num">{t.use}</th><th></th>
						</tr>
					</thead>
					<tbody>
						{#each accounts as a (a.id)}
							<tr data-account={a.id}>
								<td>{@render who(a.login, a.id, a.avatar_url)}</td>
								<td
									><span class="pill" class:bad={a.state !== 'active'}
										>{a.state === 'active' ? t.active : t.suspended}</span
									></td
								>
								<td class="muted">{@render when(a.last_seen_at * 1000)}</td>
								<td class="num">{a.apps}</td>
								<td class="num">{a.clients} <span class="muted">/ {a.client_limit}</span></td>
								<td class="num"
									>{#if a.usage}{bytes(a.usage.bytes)}<br /><span class="muted"
											>{usd(a.usage.cost)}</span
										>{:else}<span class="muted">—</span>{/if}</td
								>
								<td class="actions">
									<details class="manage">
										<summary class="button ghost small">{t.manage}</summary>
										<div class="manage-body">
											<form method="POST" action="?/account#accounts" class="limits">
												<input type="hidden" name="csrf" value={data.csrf} />
												<input type="hidden" name="account" value={a.id} />
												<label
													>{t.clientLimit}
													<input
														name="client_limit"
														type="number"
														min="0"
														value={a.client_limit}
													/></label
												>
												<label
													>{t.appLimit}
													<input
														name="app_limit"
														type="number"
														min="0"
														value={a.app_limit}
													/></label
												>
												<button class="button secondary small" type="submit">{t.saveLimits}</button>
											</form>
											{#if a.id !== data.self}
												<form
													method="POST"
													action="?/account#accounts"
													onsubmit={a.state === 'active'
														? confirmed(fill(t.confirmSuspend, { login: a.login }))
														: undefined}
												>
													<input type="hidden" name="csrf" value={data.csrf} />
													<input type="hidden" name="account" value={a.id} />
													<button
														class="button small"
														class:danger={a.state === 'active'}
														class:secondary={a.state !== 'active'}
														name="state"
														value={a.state === 'active' ? 'suspended' : 'active'}
														>{a.state === 'active' ? t.suspend : t.reactivate}</button
													>
												</form>
											{/if}
										</div>
									</details>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>
	<div
		class="panel"
		role="tabpanel"
		id="panel-apps"
		aria-labelledby="tab-apps"
		hidden={tab !== 'apps'}
	>
		<input
			class="search"
			type="search"
			placeholder={t.search}
			bind:value={query}
			aria-label={t.search}
		/>
		{#if apps.length === 0}
			<p class="empty card">{t.noMatch}</p>
		{:else}
			<div class="table card flush">
				<table>
					<thead>
						<tr
							><th>{t.tabs.apps}</th><th>{t.owner}</th><th class="num">{t.clients}</th><th
								>{t.status}</th
							><th></th></tr
						>
					</thead>
					<tbody>
						{#each apps as p (p.id)}
							<tr data-app={p.id}>
								<td><strong>{p.name}</strong><br /><code title={p.id}>{p.id}</code></td>
								<td>@{p.login}</td>
								<td class="num">{p.clients}</td>
								<td
									><span class="pill" class:bad={p.state !== 'active'}
										>{p.state === 'active' ? t.active : t.disabled}</span
									></td
								>
								<td class="actions">
									<form
										method="POST"
										action="?/app#apps"
										onsubmit={p.state === 'active'
											? confirmed(fill(t.confirmDisable, { app: p.name }))
											: undefined}
									>
										<input type="hidden" name="csrf" value={data.csrf} />
										<input type="hidden" name="app" value={p.id} />
										<button
											class="button small"
											class:danger={p.state === 'active'}
											class:secondary={p.state !== 'active'}
											name="state"
											value={p.state === 'active' ? 'disabled' : 'active'}
											>{p.state === 'active' ? t.disable : t.enable}</button
										>
									</form>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>
	<div
		class="panel"
		role="tabpanel"
		id="panel-audit"
		aria-labelledby="tab-audit"
		hidden={tab !== 'audit'}
	>
		<div class="filters">
			<div class="segmented" role="group">
				<button
					aria-pressed={source === 'site'}
					onclick={() => ((source = 'site'), (action = ''), (shown = 30))}>{t.site}</button
				>
				<button
					aria-pressed={source === 'relay'}
					onclick={() => ((source = 'relay'), (action = ''), (shown = 30))}>{t.relay}</button
				>
			</div>
			<select bind:value={action} aria-label={t.allActions}>
				<option value="">{t.allActions}</option>
				{#each actions as a (a)}<option value={a}>{said(a)}</option>{/each}
			</select>
			<input
				class="search"
				type="search"
				placeholder={t.search}
				bind:value={auditQuery}
				aria-label={t.search}
			/>
		</div>
		{#if filtered.length === 0}
			<p class="empty card">{t.noMatch}</p>
		{:else}
			<div class="table card flush">
				<table class="log">
					<tbody>
						{#each filtered.slice(0, shown) as r, i (i)}
							<tr>
								<td class="muted nowrap">{@render when(r.at)}</td>
								<td><span class="tag {tone(r.action)}" title={r.action}>{said(r.action)}</span></td>
								<td
									><strong title={r.id}>{r.subject}</strong>
									<span class="muted">{r.detail}</span></td
								>
								<td class="muted">{r.actor}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			{#if filtered.length > shown}
				<button class="button ghost small more" onclick={() => (shown += 50)}
					>{t.showMore} ({filtered.length - shown})</button
				>
			{/if}
		{/if}
	</div>
</section>

<style>
	.console {
		padding-bottom: 80px;
	}
	h1 {
		font-size: clamp(1.6rem, 2.6vw, 2.1rem);
		margin-bottom: 20px;
	}
	h2 {
		font-size: 1rem;
		margin: 0;
	}
	code {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--muted);
	}
	.muted {
		color: var(--muted);
	}
	.small-text {
		font-size: 0.82rem;
		margin: 4px 0 0;
	}
	.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.nowrap {
		white-space: nowrap;
	}

	/* Key figures */
	.kpis {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(128px, 1fr));
		gap: 12px;
		margin-bottom: 28px;
	}
	.kpi {
		display: grid;
		gap: 2px;
		grid-template-rows: auto 1fr auto;
		align-items: end;
		text-align: left;
		padding: 14px 16px;
		border: 1px solid var(--line);
		border-radius: var(--radius-sm);
		background: var(--surface);
		font: inherit;
		color: inherit;
	}
	.kpi .label {
		font-size: 0.8rem;
		color: var(--muted);
	}
	.kpi .value {
		font-size: 1.6rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
		line-height: 1.2;
	}
	.kpi .sub {
		font-size: 0.78rem;
		color: var(--danger);
		min-height: 1em;
	}
	.kpi.link {
		cursor: pointer;
	}
	.kpi.link:hover {
		border-color: var(--line-strong);
	}
	.kpi.hot {
		border-color: var(--cipher);
		background: var(--cipher-soft);
	}

	/* Tabs */
	.tabs {
		display: flex;
		gap: 4px;
		border-bottom: 1px solid var(--line);
		margin-bottom: 20px;
		overflow-x: auto;
	}
	.tabs button {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 10px 14px;
		border: none;
		border-bottom: 2px solid transparent;
		margin-bottom: -1px;
		background: none;
		font: inherit;
		font-weight: 600;
		color: var(--muted);
		cursor: pointer;
		white-space: nowrap;
	}
	.tabs button[aria-selected='true'] {
		color: var(--ink);
		border-bottom-color: var(--accent);
	}
	.badge {
		min-width: 20px;
		padding: 0 6px;
		border-radius: 999px;
		background: var(--cipher);
		color: var(--surface);
		font-size: 0.72rem;
		line-height: 20px;
		text-align: center;
	}
	.badge.quiet {
		background: var(--bg-soft);
		color: var(--muted);
	}

	/* Panels and cards */
	.panel[hidden] {
		display: none !important;
	}
	.panel.grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 16px;
	}
	.card {
		background: var(--surface);
		border: 1px solid var(--line);
		border-radius: var(--radius-sm);
		padding: 16px 18px;
		min-width: 0;
	}
	.card.wide {
		grid-column: 1 / -1;
	}
	.card.flush {
		padding: 0;
		overflow: hidden;
	}
	.card header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 12px;
		flex-wrap: wrap;
		margin-bottom: 10px;
	}
	.empty {
		color: var(--muted);
		margin: 8px 0;
	}
	p.empty.card {
		padding: 20px;
	}
	@media (max-width: 760px) {
		.panel.grid {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	/* Chart */
	.chart {
		--bar: #0a7a5e;
	}
	@media (prefers-color-scheme: dark) {
		.chart {
			--bar: #29a67e;
		}
	}
	.chart svg {
		width: 100%;
		height: 180px;
		display: block;
	}
	.readout {
		font-size: 0.85rem;
		margin: 0 0 4px;
		min-height: 1.4em;
	}
	.bar {
		fill: var(--bar);
		transition: opacity 0.1s;
	}
	.bar.dim {
		opacity: 0.35;
	}
	.hit {
		fill: transparent;
	}
	.grid-line {
		stroke: var(--line);
		stroke-width: 1;
	}
	.axis {
		fill: var(--muted);
		font-size: 11px;
		font-variant-numeric: tabular-nums;
	}
	.table-view {
		margin-top: 8px;
		font-size: 0.85rem;
	}
	.table-view summary {
		cursor: pointer;
		color: var(--muted);
	}

	/* Ranking and alerts */
	.ranking {
		list-style: none;
		padding: 0;
		margin: 0;
		counter-reset: rank;
	}
	.ranking li {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto auto;
		gap: 12px;
		padding: 8px 0;
		border-top: 1px solid var(--line);
		font-size: 0.9rem;
	}
	.ranking li:first-child {
		border-top: none;
	}
	.alerts {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	.alerts li {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto auto;
		gap: 12px;
		align-items: center;
		padding: 10px 16px;
		border-top: 1px solid var(--line);
		font-size: 0.88rem;
	}
	.alerts li:first-child {
		border-top: none;
	}
	.card:not(.flush) .alerts li {
		padding-inline: 0;
	}
	.alert-who {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	@media (max-width: 600px) {
		.alerts li {
			grid-template-columns: auto minmax(0, 1fr);
			row-gap: 4px;
		}
		.alerts li > .num {
			grid-column: 1;
			text-align: left;
		}
	}

	/* Tags and pills */
	.tag {
		display: inline-block;
		padding: 1px 8px;
		border-radius: 6px;
		background: var(--bg-soft);
		font-size: 0.78rem;
		font-weight: 600;
		white-space: nowrap;
	}
	.tag.good {
		background: var(--accent-soft);
		color: var(--accent-strong);
	}
	.tag.bad {
		background: color-mix(in srgb, var(--danger) 12%, transparent);
		color: var(--danger);
	}
	.tag.warn {
		background: var(--cipher-soft);
		color: var(--cipher);
		font-family: inherit;
		font-weight: 600;
	}
	.pill {
		display: inline-block;
		padding: 1px 10px;
		border-radius: 999px;
		background: var(--accent-soft);
		color: var(--accent-strong);
		font-size: 0.78rem;
		font-weight: 600;
	}
	.pill.bad {
		background: color-mix(in srgb, var(--danger) 14%, transparent);
		color: var(--danger);
	}

	/* Tables */
	.table {
		overflow-x: auto;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.88rem;
	}
	th {
		text-align: left;
		font-size: 0.75rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--muted);
		background: var(--bg-soft);
	}
	th,
	td {
		padding: 10px 14px;
		border-bottom: 1px solid var(--line);
		vertical-align: middle;
	}
	tbody tr:last-child td {
		border-bottom: none;
	}
	tbody tr:hover td {
		background: color-mix(in srgb, var(--bg-soft) 60%, transparent);
	}
	table.log td {
		padding: 7px 14px;
		font-size: 0.84rem;
	}
	.who {
		display: inline-flex;
		gap: 10px;
		align-items: center;
	}
	.who img,
	.who .dot {
		width: 28px;
		height: 28px;
		border-radius: 50%;
		flex: none;
	}
	.who .dot {
		background: var(--bg-soft);
	}
	.actions {
		text-align: right;
		white-space: nowrap;
	}

	/* Manage */
	.manage {
		display: inline-block;
		position: relative;
	}
	.manage summary {
		list-style: none;
	}
	.manage summary::-webkit-details-marker {
		display: none;
	}
	.manage-body {
		position: absolute;
		right: 0;
		top: calc(100% + 6px);
		z-index: 5;
		display: grid;
		gap: 12px;
		padding: 14px;
		min-width: 230px;
		text-align: left;
		background: var(--surface);
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		box-shadow: var(--shadow);
	}
	.limits {
		display: grid;
		gap: 8px;
	}
	.limits label {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 12px;
		font-size: 0.85rem;
	}
	.limits input {
		width: 80px;
		font: inherit;
		padding: 4px 8px;
		border: 1px solid var(--line-strong);
		border-radius: 6px;
		background: var(--surface);
		color: var(--ink);
	}

	/* Filters */
	.filters {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
		margin-bottom: 14px;
	}
	.search,
	select {
		font: inherit;
		font-size: 0.9rem;
		padding: 7px 12px;
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		background: var(--surface);
		color: var(--ink);
	}
	.search {
		flex: 1;
		min-width: 200px;
		margin-bottom: 14px;
		width: 100%;
	}
	.filters .search {
		margin-bottom: 0;
		width: auto;
	}
	.segmented {
		display: inline-flex;
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		overflow: hidden;
	}
	.segmented button {
		padding: 7px 14px;
		border: none;
		background: var(--surface);
		font: inherit;
		font-size: 0.88rem;
		color: var(--muted);
		cursor: pointer;
	}
	.segmented button[aria-pressed='true'] {
		background: var(--ink);
		color: var(--surface);
	}
	.more {
		margin-top: 12px;
	}
	.small {
		min-height: 32px;
		padding: 0 12px;
		font-size: 0.82rem;
	}
	.linklike {
		background: none;
		border: none;
		padding: 0;
		font: inherit;
		font-size: 0.85rem;
		color: var(--accent);
		cursor: pointer;
	}
	.note {
		display: flex;
		gap: 12px;
		align-items: center;
		border: 1px solid var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: 10px 14px;
	}
	.warn-note {
		border-color: var(--danger);
		background: transparent;
		color: var(--danger);
	}
</style>
