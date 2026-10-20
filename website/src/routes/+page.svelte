<script lang="ts">
	/* eslint svelte/no-at-html-tags: "off" -- only our own translation strings are rendered as HTML */
	import CodeTabs from '#lib/components/CodeTabs.svelte';
	import HeroDiagram from '#lib/components/HeroDiagram.svelte';
	import Seo from '#lib/components/Seo.svelte';
	import { LOCALE } from '#lib/i18n/index.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { RELAY_URL, REPO_URL, SITE_URL } from '#lib/site.ts';
	import { jsQuick, rustQuick } from '#lib/snippets.ts';

	const t = $derived(i18n.t);
	const h = $derived(t.home);

	// Simple 24×24 stroke icons, one per feature.
	const icons = [
		'M7 11V8a5 5 0 0 1 10 0v3M6 11h12v9H6z M12 15v2',
		'M4 6h16v5H4zM4 13h16v5H4z M8 8.5h.01M8 15.5h.01',
		'M3 12l4 4L17 6 M10 16l1 1 10-10',
		'M10 14a4 4 0 0 1 0-5.6l2-2a4 4 0 0 1 5.6 5.6l-1 1 M14 10a4 4 0 0 1 0 5.6l-2 2a4 4 0 0 1-5.6-5.6l1-1',
		'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9s1.3-6.4 3.8-9z',
		'M7 3h7l4 4v14H7z M14 3v4h4 M10 13l2 2 3-4'
	];

	const jsonLd = $derived([
		{
			'@context': 'https://schema.org',
			'@type': 'WebSite',
			name: 'PeerConnect',
			url: SITE_URL + i18n.href('/').replace(/^\/$/, '/'),
			inLanguage: LOCALE[i18n.lang].tag,
			description: t.meta.home.description
		},
		{
			'@context': 'https://schema.org',
			'@type': 'SoftwareSourceCode',
			name: 'PeerConnect',
			description: t.meta.home.description,
			url: SITE_URL,
			programmingLanguage: ['Rust', 'JavaScript', 'WebAssembly'],
			license: [
				'https://opensource.org/licenses/MIT',
				'https://www.apache.org/licenses/LICENSE-2.0'
			],
			...(REPO_URL ? { codeRepository: REPO_URL } : {})
		},
		{
			'@context': 'https://schema.org',
			'@type': 'FAQPage',
			inLanguage: LOCALE[i18n.lang].tag,
			mainEntity: h.faq.items.map((item) => ({
				'@type': 'Question',
				name: item.q,
				acceptedAnswer: { '@type': 'Answer', text: item.a }
			}))
		}
	]);
</script>

<Seo
	title={t.meta.home.title}
	description={t.meta.home.description}
	path="/"
	lang={i18n.lang}
	{jsonLd}
/>

<section class="hero">
	<div class="container hero-inner">
		<span class="eyebrow">{h.hero.eyebrow}</span>
		<h1>{@html h.hero.title}</h1>
		<p class="lead">{h.hero.lead}</p>
		<div class="actions">
			<a class="button primary" href={i18n.href('/demo')}>
				{h.hero.primary}
				<svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
					<path
						d="M5 12h14M13 6l6 6-6 6"
						stroke="currentColor"
						stroke-width="2"
						fill="none"
						stroke-linecap="round"
						stroke-linejoin="round"
					/>
				</svg>
			</a>
			<a class="button secondary" href={i18n.href('/docs')}>{h.hero.secondary}</a>
		</div>
		<ul class="facts">
			{#each h.hero.facts as [value, label] (label)}
				<li><strong>{value}</strong> {label}</li>
			{/each}
		</ul>
	</div>
	<div class="container diagram">
		<HeroDiagram t={h.diagram} />
	</div>
</section>

<section class="section" id="features">
	<div class="container">
		<div class="section-head">
			<span class="eyebrow">{h.features.eyebrow}</span>
			<h2>{h.features.title}</h2>
			<p>{h.features.intro}</p>
		</div>
		<div class="features">
			{#each h.features.items as item, i (item.title)}
				<article class="feature">
					<span class="icon">
						<svg width="22" height="22" viewBox="0 0 24 24" aria-hidden="true">
							<path
								d={icons[i]}
								fill="none"
								stroke="currentColor"
								stroke-width="1.7"
								stroke-linecap="round"
								stroke-linejoin="round"
							/>
						</svg>
					</span>
					<h3>{item.title}</h3>
					<p>{@html item.text}</p>
				</article>
			{/each}
		</div>
	</div>
</section>

<section class="section alt" id="how-it-works">
	<div class="container">
		<div class="section-head">
			<span class="eyebrow">{h.how.eyebrow}</span>
			<h2>{h.how.title}</h2>
			<p>{h.how.intro}</p>
		</div>
		<ol class="steps">
			{#each h.how.steps as step, i (step.title)}
				<li class="step">
					<span class="number">{String(i + 1).padStart(2, '0')}</span>
					<h3>{step.title}</h3>
					<p>{step.text}</p>
					<code class="detail">{step.detail}</code>
				</li>
			{/each}
		</ol>
	</div>
</section>

<section class="section" id="code">
	<div class="container split">
		<div class="section-head sticky">
			<span class="eyebrow">{h.code.eyebrow}</span>
			<h2>{h.code.title}</h2>
			<p>{h.code.intro}</p>
			<p class="note">{@html h.code.note.replace('{docs}', i18n.href('/docs'))}</p>
		</div>
		<CodeTabs
			id="quick"
			tabs={[
				{ label: 'JavaScript', code: jsQuick(i18n.lang), lang: 'js', title: 'app.js' },
				{ label: 'Rust', code: rustQuick(i18n.lang), lang: 'rust', title: 'main.rs' }
			]}
		/>
	</div>
</section>

<section class="section alt" id="self-hosting">
	<div class="container">
		<div class="section-head">
			<span class="eyebrow">{h.relay.eyebrow}</span>
			<h2>{h.relay.title}</h2>
			<p>{h.relay.intro}</p>
		</div>
		<div class="relays">
			{#each h.relay.cards as card (card.title)}
				<article class="card relay-card">
					<h3>{card.title}</h3>
					<p>{@html card.text}</p>
				</article>
			{/each}
		</div>
		<div class="public">
			<p>{@html h.relay.public.replace('{relay}', RELAY_URL)}</p>
			<a class="button secondary" href={i18n.href('/docs') + '#relay'}>{h.relay.guide}</a>
		</div>
	</div>
</section>

<section class="section" id="security">
	<div class="container">
		<div class="section-head">
			<span class="eyebrow">{h.security.eyebrow}</span>
			<h2>{h.security.title}</h2>
		</div>
		<div class="ledger">
			<div class="card column">
				<h3>{h.security.seesTitle}</h3>
				<ul>
					{#each h.security.sees as item (item)}
						<li class="sees">{item}</li>
					{/each}
				</ul>
			</div>
			<div class="card column">
				<h3>{h.security.notTitle}</h3>
				<ul>
					{#each h.security.not as item (item)}
						<li class="never">{item}</li>
					{/each}
				</ul>
			</div>
		</div>
		<div class="status">
			<p>{@html h.security.status}</p>
			<a href={i18n.href('/docs') + '#limitations'}>{h.security.limitations} →</a>
		</div>
	</div>
</section>

<section class="section alt" id="use-cases">
	<div class="container">
		<div class="section-head">
			<span class="eyebrow">{h.uses.eyebrow}</span>
			<h2>{h.uses.title}</h2>
		</div>
		<div class="uses">
			{#each h.uses.items as item (item.title)}
				<article class="use">
					<h3>{item.title}</h3>
					<p>{item.text}</p>
				</article>
			{/each}
		</div>
	</div>
</section>

<section class="section" id="faq">
	<div class="container split">
		<div class="section-head">
			<span class="eyebrow">{h.faq.eyebrow}</span>
			<h2>{h.faq.title}</h2>
		</div>
		<div class="faq">
			{#each h.faq.items as item (item.q)}
				<details>
					<summary>{item.q}</summary>
					<p>{item.a}</p>
				</details>
			{/each}
		</div>
	</div>
</section>

<section class="cta">
	<div class="container cta-inner">
		<h2>{h.cta.title}</h2>
		<p>{h.cta.text}</p>
		<a class="button" href={i18n.href('/demo')}>{h.cta.button}</a>
	</div>
</section>

<style>
	.hero {
		position: relative;
		padding-block: clamp(56px, 9vw, 112px) clamp(24px, 4vw, 48px);
		overflow: hidden;
	}

	.hero::before {
		content: '';
		position: absolute;
		inset: 0;
		z-index: -1;
		background-image: radial-gradient(var(--line-strong) 1px, transparent 1px);
		background-size: 22px 22px;
		mask-image: radial-gradient(ellipse 70% 60% at 50% 0%, #000 30%, transparent 75%);
		opacity: 0.7;
	}

	.hero-inner {
		display: grid;
		justify-items: center;
		gap: 22px;
		text-align: center;
	}

	h1 {
		font-size: clamp(2.5rem, 6.4vw, 4.6rem);
		font-weight: 720;
		letter-spacing: -0.035em;
		line-height: 1.02;
	}

	:global(:lang(zh)) h1 {
		font-size: clamp(2.2rem, 5.4vw, 3.9rem);
		letter-spacing: 0;
		line-height: 1.2;
	}

	.lead {
		max-width: 680px;
		color: var(--ink-2);
		font-size: clamp(1.06rem, 1.8vw, 1.22rem);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 12px;
		margin-top: 6px;
	}

	.facts {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 8px 28px;
		margin: 10px 0 0;
		padding: 0;
		list-style: none;
		color: var(--muted);
		font-size: 0.92rem;
	}

	.facts strong {
		color: var(--ink);
		font-family: var(--font-mono);
		font-size: 1.05rem;
		margin-right: 2px;
	}

	.diagram {
		max-width: calc(980px + 2 * var(--gutter));
		margin-top: clamp(40px, 6vw, 64px);
	}

	.alt {
		background: var(--bg-soft);
		border-block: 1px solid var(--line);
	}

	h3 {
		font-size: 1.12rem;
	}

	.features {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 1px;
		overflow: hidden;
		border: 1px solid var(--line);
		border-radius: var(--radius);
		background: var(--line);
	}

	.feature {
		display: grid;
		align-content: start;
		gap: 10px;
		padding: clamp(22px, 3vw, 32px);
		background: var(--surface);
	}

	.feature p,
	.step p,
	.relay-card p,
	.use p {
		color: var(--muted);
		font-size: 0.97rem;
	}

	.icon {
		display: grid;
		place-items: center;
		width: 42px;
		height: 42px;
		margin-bottom: 6px;
		border-radius: 12px;
		background: var(--accent-soft);
		color: var(--accent);
	}

	.steps {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 20px;
		margin: 0;
		padding: 0;
		list-style: none;
		counter-reset: step;
	}

	.step {
		position: relative;
		display: grid;
		grid-template-rows: auto auto 1fr auto;
		gap: 10px;
		padding-top: 22px;
		border-top: 2px solid var(--line-strong);
	}

	.step:first-child {
		border-top-color: var(--accent);
	}

	.number {
		font-family: var(--font-mono);
		font-size: 0.85rem;
		color: var(--accent);
		font-weight: 600;
	}

	.detail {
		justify-self: start;
		margin-top: 6px;
		font-size: 0.78rem;
		color: var(--cipher);
		background: var(--cipher-soft);
		border-color: transparent;
	}

	.split {
		display: grid;
		grid-template-columns: minmax(0, 5fr) minmax(0, 7fr);
		gap: clamp(32px, 6vw, 72px);
		align-items: start;
	}

	.split .section-head {
		margin-bottom: 0;
	}

	.sticky {
		position: sticky;
		top: calc(var(--header-h) + 32px);
	}

	.note {
		font-size: 0.95rem !important;
	}

	.relays {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 20px;
	}

	.relay-card {
		display: grid;
		align-content: start;
		gap: 10px;
		padding: 28px;
	}

	.public {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
		margin-top: 24px;
		padding: 20px 24px;
		border: 1px dashed var(--line-strong);
		border-radius: var(--radius);
		color: var(--ink-2);
	}

	.ledger {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 20px;
	}

	.column {
		padding: 28px;
	}

	.column h3 {
		margin-bottom: 16px;
	}

	.column ul {
		display: grid;
		gap: 12px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.column li {
		position: relative;
		padding-left: 30px;
		color: var(--ink-2);
	}

	.column li::before {
		position: absolute;
		left: 0;
		top: 0.2em;
		display: grid;
		place-items: center;
		width: 20px;
		height: 20px;
		border-radius: 6px;
		font-size: 0.72rem;
		font-weight: 700;
	}

	.sees::before {
		content: '~';
		background: var(--cipher-soft);
		color: var(--cipher);
	}

	.never::before {
		content: '✕';
		background: var(--accent-soft);
		color: var(--accent);
	}

	.status {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px 24px;
		flex-wrap: wrap;
		margin-top: 20px;
		padding: 20px 24px;
		border-radius: var(--radius);
		background: var(--cipher-soft);
		color: var(--ink-2);
		font-size: 0.97rem;
	}

	.status a {
		white-space: nowrap;
		font-weight: 600;
	}

	.uses {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 20px;
	}

	.use {
		display: grid;
		align-content: start;
		gap: 10px;
		padding-top: 18px;
		border-top: 1px solid var(--line-strong);
	}

	.faq {
		border-top: 1px solid var(--line);
	}

	details {
		border-bottom: 1px solid var(--line);
	}

	summary {
		position: relative;
		padding: 20px 40px 20px 0;
		font-weight: 600;
		font-size: 1.05rem;
		cursor: pointer;
		list-style: none;
	}

	summary::-webkit-details-marker {
		display: none;
	}

	summary::after {
		content: '+';
		position: absolute;
		right: 4px;
		top: 50%;
		transform: translateY(-50%);
		color: var(--muted);
		font-size: 1.4rem;
		font-weight: 400;
		transition: transform 0.2s;
	}

	details[open] summary::after {
		transform: translateY(-50%) rotate(45deg);
	}

	details p {
		padding-bottom: 22px;
		color: var(--muted);
	}

	.cta {
		padding-block: clamp(64px, 9vw, 104px);
		background: var(--accent);
		color: var(--accent-ink);
	}

	.cta-inner {
		display: grid;
		justify-items: center;
		gap: 18px;
		text-align: center;
	}

	.cta h2 {
		font-size: clamp(1.9rem, 4vw, 2.8rem);
	}

	.cta p {
		max-width: 620px;
		opacity: 0.9;
		font-size: 1.08rem;
	}

	.cta .button {
		margin-top: 6px;
		background: var(--accent-ink);
		color: var(--accent);
	}

	.cta .button:hover {
		opacity: 0.92;
	}

	@media (max-width: 960px) {
		.features {
			grid-template-columns: repeat(2, 1fr);
		}

		.steps,
		.uses {
			grid-template-columns: repeat(2, 1fr);
			row-gap: 32px;
		}

		.split {
			grid-template-columns: 1fr;
		}

		.sticky {
			position: static;
		}

		.relays {
			grid-template-columns: 1fr;
		}
	}

	@media (max-width: 640px) {
		.features,
		.steps,
		.uses,
		.ledger {
			grid-template-columns: 1fr;
		}

		.public {
			flex-direction: column;
			align-items: flex-start;
		}

		.hero-inner {
			justify-items: start;
			text-align: left;
		}

		.actions,
		.facts {
			justify-content: flex-start;
		}

		.actions .button {
			flex: 1 1 auto;
		}
	}
</style>
