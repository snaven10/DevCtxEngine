import { AuthService } from '@acme/auth';

function inject<T>(t: T): T {
  return t;
}

export function useLocal(): void {
  const s = inject(AuthService);
  s.token();
}
