import { renderSpec } from '#lib/server/spec.ts';
import api from '../../../../../API.md?raw';
import type { PageServerLoad } from './$types';

// The API reference is normative and exists in English only, like the specification.
export const load: PageServerLoad = () => renderSpec(api);
