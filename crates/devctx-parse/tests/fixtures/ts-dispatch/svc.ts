export interface IService {
  update(id: string): void;
}

export abstract class BaseService implements IService {
  abstract update(id: string): void;
}

export class ServiceImpl extends BaseService {
  update(id: string): void {}
}
