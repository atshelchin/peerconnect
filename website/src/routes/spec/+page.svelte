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

<SpecDocument html={data.html} toc={data.toc} tocLabel={t.docs.toc}>
	<p class="next">
		Groups of up to 32 members have their own protocol: <a href="/spec/groups"
			>PeerConnect Groups, version 1</a
		>. For how to use them, see the <a href="/groups">groups guide</a>.
	</p>
</SpecDocument>
