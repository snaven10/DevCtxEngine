// A second `AuthService`: only the import says which one a file means.
export class AuthService {
  token(): string {
    return 'data';
  }

  login(user: string): string {
    return user;
  }
}
