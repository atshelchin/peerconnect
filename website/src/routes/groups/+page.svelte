<script lang="ts">
	import Prose from '#lib/components/Prose.svelte';
	import Seo from '#lib/components/Seo.svelte';
	import { LOCALE } from '#lib/i18n/index.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { SITE_URL } from '#lib/site.ts';
	import GroupsEn, { toc as tocEn } from './GroupsEn.svelte';
	import GroupsZh, { toc as tocZh } from './GroupsZh.svelte';

	const t = $derived(i18n.t);
	const toc = $derived(i18n.lang === 'zh' ? tocZh : tocEn);
	const jsonLd = $derived([
		{
			'@context': 'https://schema.org',
			'@type': 'TechArticle',
			headline: t.meta.groups.title,
			description: t.meta.groups.description,
			inLanguage: LOCALE[i18n.lang].tag,
			url: SITE_URL + i18n.href('/groups'),
			about: { '@type': 'SoftwareSourceCode', name: 'PeerConnect' }
		}
	]);
</script>

<Seo
	title={t.meta.groups.title}
	description={t.meta.groups.description}
	path="/groups"
	lang={i18n.lang}
	type="article"
	{jsonLd}
/>

<Prose {toc} tocLabel={t.docs.toc}>
	{#if i18n.lang === 'zh'}
		<GroupsZh />
	{:else}
		<GroupsEn />
	{/if}
</Prose>
