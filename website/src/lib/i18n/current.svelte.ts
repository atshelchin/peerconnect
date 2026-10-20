import { page } from '$app/state';
import { dict, langOf, localize, unlocalize, type Lang } from './index';

/** The language of the page being shown, and its strings. Reactive. */
export const i18n = {
	get lang(): Lang {
		return langOf(page.url.pathname);
	},
	get t() {
		return dict(this.lang);
	},
	/** The current page's path without its language prefix. */
	get path(): string {
		return unlocalize(page.url.pathname);
	},
	/** A link to a site path, in the current language. */
	href(path: string): string {
		return localize(path, this.lang);
	}
};
