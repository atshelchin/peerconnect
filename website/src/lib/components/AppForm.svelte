<script lang="ts">
	// An application's settings: registration, and its settings form.
	import { consoleText } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';

	let {
		action,
		csrf,
		submit,
		values = { name: '', redirect_uris: '', origins: '', loopback_path: '' }
	}: {
		action: string;
		csrf: string;
		submit: string;
		values?: { name: string; redirect_uris: string; origins: string; loopback_path: string };
	} = $props();
	const d = $derived(consoleText[i18n.lang].developers);
</script>

<form class="app-form" method="POST" {action}>
	<input type="hidden" name="csrf" value={csrf} />
	<label>
		<span>{d.name}</span>
		<input
			name="name"
			maxlength="40"
			required
			placeholder={d.namePlaceholder}
			value={values.name}
		/>
	</label>
	<label>
		<span>{d.redirects}</span>
		<textarea name="redirect_uris" rows="3" spellcheck="false">{values.redirect_uris}</textarea>
		<small>{d.redirectsHint}</small>
	</label>
	<label>
		<span>{d.origins}</span>
		<textarea name="origins" rows="2" spellcheck="false">{values.origins}</textarea>
		<small>{d.originsHint}</small>
	</label>
	<label>
		<span>{d.loopback}</span>
		<input
			name="loopback_path"
			maxlength="100"
			placeholder="/peerconnect"
			value={values.loopback_path}
		/>
		<small>{d.loopbackHint}</small>
	</label>
	<button class="button primary" type="submit">{submit}</button>
</form>

<style>
	.app-form {
		display: grid;
		gap: 18px;
		max-width: 640px;
	}
	label {
		display: grid;
		gap: 6px;
	}
	label > span {
		font-weight: 600;
	}
	small {
		color: var(--muted);
		font-size: 0.85rem;
	}
	input,
	textarea {
		font: inherit;
		font-size: 0.95rem;
		padding: 8px 12px;
		border: 1px solid var(--line-strong);
		border-radius: var(--radius-sm);
		background: var(--surface);
		color: var(--ink);
		min-width: 0;
	}
	textarea {
		font-family: var(--font-mono);
		font-size: 0.85rem;
		resize: vertical;
	}
	button {
		justify-self: start;
	}
</style>
