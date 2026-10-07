import os
import logging as log
from .models import Order, Line as L
from typing import *

LIMIT = 10


class OrderService(BaseService, mixins.Audited):
    def __init__(self, repo: Repo):
        self.repo = repo

    def find(self, oid: int) -> Order:
        return self.repo.get(oid)

    def _hidden(self):
        pass


def main():
    OrderService(None).find(1)


main()
