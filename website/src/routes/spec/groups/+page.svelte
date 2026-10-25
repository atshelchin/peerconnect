<script lang="ts">
	import Seo from '#lib/components/Seo.svelte';
	import SpecDocument from '#lib/components/SpecDocument.svelte';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { SITE_URL } from '#lib/site.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const t = $derived(i18n.t);
	const jsonLd = $derived([
		{
			'@context': 'https://schema.org',
			'@type': 'TechArticle',
			headline: 'PeerConnect Groups, version 1',
			description: t.meta.specGroups.description,
			inLanguage: 'en',
			url: `${SITE_URL}/spec/groups`,
			isPartOf: { '@type': 'TechArticle', url: `${SITE_URL}/spec` },
			about: { '@type': 'SoftwareSourceCode', name: 'PeerConnect' }
		}
	]);
</script>

<Seo
	title={t.meta.specGroups.title}
	description={t.meta.specGroups.description}
	path="/spec/groups"
	lang="en"
	translated={false}
	type="article"
	{jsonLd}
/>

<SpecDocument html={data.html} toc={data.toc} tocLabel={t.docs.toc}>
	<p class="next">
		Two-peer rooms are specified in the <a href="/spec">PeerConnect Protocol, version 1</a>. For how
		to build on groups, with examples in JavaScript, Rust and C, see the
		<a href="/groups">groups guide</a>.
	</p>
</SpecDocument>
