export class Store {
  save(): void {
    localStorage.setItem('k', 'v');
  }
}

export default class DefaultStore {
  clear(): void {}
}
