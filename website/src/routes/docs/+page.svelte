<script lang="ts">
	import Prose from '#lib/components/Prose.svelte';
	import Seo from '#lib/components/Seo.svelte';
	import { LOCALE } from '#lib/i18n/index.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { SITE_URL } from '#lib/site.ts';
	import DocsEn, { toc as tocEn } from './DocsEn.svelte';
	import DocsZh, { toc as tocZh } from './DocsZh.svelte';

	const t = $derived(i18n.t);
	const toc = $derived(i18n.lang === 'zh' ? tocZh : tocEn);
	const jsonLd = $derived([
		{
			'@context': 'https://schema.org',
			'@type': 'TechArticle',
			headline: t.meta.docs.title,
			description: t.meta.docs.description,
			inLanguage: LOCALE[i18n.lang].tag,
			url: SITE_URL + i18n.href('/docs'),
			about: { '@type': 'SoftwareSourceCode', name: 'PeerConnect' }
		}
	]);
</script>

<Seo
	title={t.meta.docs.title}
	description={t.meta.docs.description}
	path="/docs"
	lang={i18n.lang}
	type="article"
	{jsonLd}
/>

<Prose {toc} tocLabel={t.docs.toc}>
	{#if i18n.lang === 'zh'}
		<DocsZh />
	{:else}
		<DocsEn />
	{/if}
</Prose>
