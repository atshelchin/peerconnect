<script lang="ts">
	import Prose from '#lib/components/Prose.svelte';
	import Seo from '#lib/components/Seo.svelte';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { SITE_URL } from '#lib/site.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const t = $derived(i18n.t);
	const jsonLd = $derived([
		{
			'@context': 'https://schema.org',
			'@type': 'TechArticle',
			headline: 'PeerConnect Protocol, version 1',
			description: t.meta.spec.description,
			inLanguage: 'en',
			url: `${SITE_URL}/spec`,
			about: { '@type': 'SoftwareSourceCode', name: 'PeerConnect' }
		}
	]);
</script>

<Seo
	title={t.meta.spec.title}
	description={t.meta.spec.description}
	path="/spec"
	lang="en"
	translated={false}
	type="article"
	{jsonLd}
/>

<Prose toc={data.toc} tocLabel={t.docs.toc}>
	<div class="spec">
		<!-- eslint-disable-next-line svelte/no-at-html-tags -- rendered at build time from SPEC.md in this repository -->
		{@html data.html}
	</div>
</Prose>

<style>
	.spec :global(.anchor) {
		float: left;
		margin-left: -1.1em;
		padding-right: 0.3em;
		color: var(--line-strong);
		text-decoration: none;
		opacity: 0;
		transition: opacity 0.15s;
	}

	.spec :global(:is(h2, h3, h4):hover .anchor),
	.spec :global(.anchor:focus) {
		opacity: 1;
	}

	.spec :global(h1 + p) {
		color: var(--ink-2);
		font-size: 1.15rem;
	}
</style>
