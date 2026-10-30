-- Test database for the Docker PostgreSQL (cobalt-postgres, localhost:5433, postgres / Cobalt!Dev2026pw).
-- Mirrors tests/seed.sql in spirit: assorted types, keys, a view, a function, a sequence, a big table.
CREATE SCHEMA IF NOT EXISTS shop;

CREATE TABLE IF NOT EXISTS public.all_types (
    id            integer PRIMARY KEY,
    b             boolean,
    si            smallint,
    bi            bigint,
    r             real,
    f             double precision,
    n             numeric(12, 3),
    t             text,
    vc            varchar(40),
    ch            char(5),
    d             date,
    ts            timestamp,
    tz            timestamptz,
    tm            time,
    u             uuid,
    j             jsonb,
    by            bytea,
    arr           integer[],
    iv            interval
);
INSERT INTO public.all_types VALUES
    (1, true, 7, 9007199254740993, 1.5, 2.25, 1234.567, 'text one', 'varchar one', 'abc', DATE '2024-02-29', TIMESTAMP '2024-02-29 13:45:30.123456', TIMESTAMPTZ '2024-02-29 13:45:30+02', TIME '13:45:30', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '{"k": [1, 2, {"x": null}]}', '\x00ff10', ARRAY[1, 2, 3], INTERVAL '1 day 02:03:04'),
    (2, false, NULL, NULL, NULL, NULL, -0.001, NULL, '', 'xy', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
    (3, NULL, -32768, -1, 0, -1e300, 99999999.999, 'ünïcödé ✓ 日本語', 'q"uote', 'a', DATE '1999-12-31', TIMESTAMP '1999-12-31 23:59:59', TIMESTAMPTZ '2000-01-01 00:00:00+00', TIME '00:00:00', '00000000-0000-0000-0000-000000000000', '"a string"', '\x', ARRAY[]::integer[], INTERVAL '0');

CREATE TABLE IF NOT EXISTS shop.customers (
    id      serial PRIMARY KEY,
    name    text NOT NULL,
    email   text UNIQUE,
    city    text DEFAULT 'Reno',
    created date
);
INSERT INTO shop.customers (name, email, city, created) VALUES
    ('Ann', 'ann@example.com', 'Boise', '2024-01-05'),
    ('Bob', 'bob@example.com', 'Reno', '2024-02-06'),
    ('Cy', 'cy@example.com', 'Boise', NULL),
    ('Dee', 'dee@example.com', 'Tucson', '2024-04-08'),
    ('Eve', 'eve@example.com', 'Boise', '2024-05-09');

CREATE TABLE IF NOT EXISTS shop.orders (
    id          bigserial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES shop.customers (id),
    amount      numeric(10, 2) NOT NULL,
    status      text NOT NULL CHECK (status IN ('new', 'shipped', 'cancelled')),
    created     timestamptz NOT NULL DEFAULT now()
);
INSERT INTO shop.orders (customer_id, amount, status, created)
SELECT (g % 5) + 1, round((random() * 500)::numeric, 2), (ARRAY['new', 'shipped', 'cancelled'])[(g % 3) + 1], now() - (g || ' hours')::interval
FROM generate_series(1, 2001) AS g;
CREATE INDEX IF NOT EXISTS orders_customer_idx ON shop.orders (customer_id, created DESC);

CREATE OR REPLACE VIEW shop.v_customer_totals AS
SELECT c.id, c.name, count(o.id) AS orders, coalesce(sum(o.amount), 0) AS total
FROM shop.customers c LEFT JOIN shop.orders o ON o.customer_id = c.id
GROUP BY c.id, c.name;

CREATE OR REPLACE FUNCTION shop.order_total(p_customer integer) RETURNS numeric
LANGUAGE sql STABLE AS $$
    SELECT coalesce(sum(amount), 0) FROM shop.orders WHERE customer_id = p_customer; -- a semicolon inside dollar quotes
$$;

CREATE OR REPLACE PROCEDURE shop.touch_customer(p_id integer)
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE shop.customers SET city = city WHERE id = p_id;
END;
$$;

CREATE SEQUENCE IF NOT EXISTS shop.ticket_seq START 100;

CREATE TABLE IF NOT EXISTS public.big (
    id       integer PRIMARY KEY,
    category varchar(10) NOT NULL,
    amount   numeric(12, 2) NOT NULL,
    created  timestamp NOT NULL,
    note     text
);
INSERT INTO public.big
SELECT g, 'cat' || (g % 7), (g % 1000) / 7.0, TIMESTAMP '2020-01-01' + (g || ' seconds')::interval, CASE WHEN g % 10 = 0 THEN NULL ELSE 'note ' || g END
FROM generate_series(1, 200000) AS g
ON CONFLICT DO NOTHING;
