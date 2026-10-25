<script lang="ts">
	/* eslint svelte/no-at-html-tags: "off" -- only our own translation strings are rendered as HTML */
	import Seo from '#lib/components/Seo.svelte';
	import { LOCALE } from '#lib/i18n/index.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { RELAY_URL, SITE_URL } from '#lib/site.ts';
	import Demo from './Demo.svelte';

	const t = $derived(i18n.t);
	const jsonLd = $derived([
		{
			'@context': 'https://schema.org',
			'@type': 'WebApplication',
			name: t.meta.demo.title,
			description: t.meta.demo.description,
			url: SITE_URL + i18n.href('/demo'),
			inLanguage: LOCALE[i18n.lang].tag,
			applicationCategory: 'CommunicationApplication',
			operatingSystem: 'Any (web browser)',
			offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' }
		}
	]);
</script>

<Seo
	title={t.meta.demo.title}
	description={t.meta.demo.description}
	path="/demo"
	lang={i18n.lang}
	{jsonLd}
/>

<section class="container demo">
	<header class="head">
		<span class="eyebrow">{t.demo.eyebrow}</span>
		<h1>{t.demo.title}</h1>
		<p class="lead">{t.demo.lead}</p>
		<ol class="steps">
			{#each t.demo.steps as step, i (i)}
				<li><span>{i + 1}</span>{step}</li>
			{/each}
		</ol>
	</header>

	<noscript><p class="card noscript">{t.demo.noscript}</p></noscript>
	<Demo />

	<p class="note">{@html t.demo.note.replace('{relay}', RELAY_URL)}</p>
	<p class="note"><a href={i18n.href('/demo/voice')}>{t.demo.voiceLink}</a></p>
</section>

<style>
	.demo {
		padding-block: clamp(40px, 6vw, 72px) clamp(64px, 9vw, 112px);
	}

	.head {
		display: grid;
		gap: 14px;
		max-width: 760px;
		margin-bottom: clamp(28px, 4vw, 40px);
	}

	h1 {
		font-size: clamp(2.1rem, 4.6vw, 3rem);
	}

	.lead {
		color: var(--ink-2);
		font-size: 1.12rem;
	}

	.steps {
		display: grid;
		gap: 8px;
		margin: 6px 0 0;
		padding: 0;
		list-style: none;
		color: var(--ink-2);
		font-size: 0.97rem;
	}

	.steps li {
		display: flex;
		gap: 12px;
	}

	.steps span {
		flex: none;
		display: grid;
		place-items: center;
		width: 24px;
		height: 24px;
		margin-top: 2px;
		border-radius: 50%;
		background: var(--accent-soft);
		color: var(--accent);
		font-family: var(--font-mono);
		font-size: 0.78rem;
		font-weight: 700;
	}

	.noscript {
		padding: 24px;
	}

	.note {
		max-width: 760px;
		margin-top: 24px;
		color: var(--muted);
		font-size: 0.92rem;
	}
</style>
