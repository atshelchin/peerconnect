import type { Lang } from './index';

// The language a visitor chose, kept in this browser only. Storage may be
// unavailable (private windows, blocked site data): everything works without it.

const KEY = 'peerconnect:lang';

export function rememberLang(lang: Lang): void {
	try {
		localStorage.setItem(KEY, lang);
	} catch {
		// not available
	}
}

export function rememberedLang(): Lang | null {
	try {
		const value = localStorage.getItem(KEY);
		return value === 'en' || value === 'zh' ? value : null;
	} catch {
		return null;
	}
}

/** The visitor's preferred language: their earlier choice, else the browser's. */
export function preferredLang(): Lang {
	const remembered = rememberedLang();
	if (remembered) return remembered;
	const first = (navigator.languages?.[0] ?? navigator.language ?? 'en').toLowerCase();
	return first.startsWith('zh') ? 'zh' : 'en';
}
