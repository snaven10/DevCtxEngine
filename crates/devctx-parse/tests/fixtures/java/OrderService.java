package com.example.shop;

import java.util.List;
import java.util.*;
import static java.util.Objects.requireNonNull;
import com.example.shop.repo.OrderRepository;

/** Places and finds orders. */
public class OrderService extends BaseService implements Auditable, Serializable {
    @Inject
    OrderRepository repository;
    private static final Logger LOG = Logger.getLogger("orders");
    private final List<Order> cache = new ArrayList<>();

    public OrderService(OrderRepository repository) {
        this.repository = repository;
        init();
    }

    public Order find(Long id,
                      String user) {
        Order found = repository.findById(id);
        var copy = found;
        return found;
    }

    public record Line(String sku, int qty) implements Comparable<Line> {
        public int compareTo(Line other) { return 0; }
    }

    static class Cache {
        void clear() { new Order(); }
    }

    private void init() {
        new Runnable() { public void run() { flush(); } };
    }
}

interface Auditable extends Named {
    String audit();
}
