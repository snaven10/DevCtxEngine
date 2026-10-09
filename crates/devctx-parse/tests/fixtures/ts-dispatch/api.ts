import { IService, BaseService } from './svc';

export class Api {
  constructor(private readonly service: IService, private readonly base: BaseService) {}

  handle(): void {
    this.service.update('x');
    this.base.update('y');
  }
}
