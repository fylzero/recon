#!/usr/bin/env bun
/**
 * Demo database generator for Recon screenshot automation.
 * Creates a realistic SQLite database with:
 * - Small e-commerce schema (customers, orders, products, order_items)
 * - Polymorphic relationships (attachments table with attachable_type and attachable_id)
 * - Deliberately missing foreign keys (order_items.product_id has no FK constraint)
 * - Believable fake data (no real people or companies)
 */

import { Database } from "bun:sqlite";
import * as path from "path";
import * as fs from "fs";

const TEMP_DIR = process.env.RECON_DEMO_DB_DIR || "/tmp/recon-demo";
const DB_PATH = path.join(TEMP_DIR, "demo.db");

// Ensure temp directory exists
if (!fs.existsSync(TEMP_DIR)) {
  fs.mkdirSync(TEMP_DIR, { recursive: true });
}

// Remove existing database
if (fs.existsSync(DB_PATH)) {
  fs.unlinkSync(DB_PATH);
}

console.log(`Creating demo database at ${DB_PATH}...`);

const db = new Database(DB_PATH);

// Enable foreign keys (but we'll deliberately skip some)
db.run("PRAGMA foreign_keys = ON");

// Create schema
db.run(`
  CREATE TABLE customers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    full_name TEXT NOT NULL,
    city TEXT,
    country TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

db.run(`
  CREATE TABLE products (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sku TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    price_cents INTEGER NOT NULL,
    stock_quantity INTEGER NOT NULL DEFAULT 0,
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

db.run(`
  CREATE TABLE orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id INTEGER NOT NULL,
    order_number TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'pending',
    total_cents INTEGER NOT NULL,
    notes TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (customer_id) REFERENCES customers(id)
  )
`);

db.run(`
  CREATE INDEX idx_orders_customer_id ON orders(customer_id)
`);

db.run(`
  CREATE INDEX idx_orders_status ON orders(status)
`);

// Deliberately NO foreign key constraint on product_id to demonstrate missing FK detection
db.run(`
  CREATE TABLE order_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id INTEGER NOT NULL,
    product_id INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    price_cents INTEGER NOT NULL,
    FOREIGN KEY (order_id) REFERENCES orders(id)
  )
`);

db.run(`
  CREATE INDEX idx_order_items_order_id ON order_items(order_id)
`);

// Polymorphic table: attachments can belong to customers, products, or orders
db.run(`
  CREATE TABLE attachments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attachable_type TEXT NOT NULL,
    attachable_id INTEGER NOT NULL,
    filename TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    mime_type TEXT NOT NULL,
    uploaded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

db.run(`
  CREATE INDEX idx_attachments_polymorphic ON attachments(attachable_type, attachable_id)
`);

// Another polymorphic table: comments can be on products or orders
db.run(`
  CREATE TABLE comments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    commentable_type TEXT NOT NULL,
    commentable_id INTEGER NOT NULL,
    author_name TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

db.run(`
  CREATE INDEX idx_comments_polymorphic ON comments(commentable_type, commentable_id)
`);

console.log("Schema created. Inserting demo data...");

// Insert customers
const customers = [
  { email: "alice.winters@mailbox.local", name: "Alice Winters", city: "Portland", country: "US" },
  { email: "bob.chen@webmail.test", name: "Bob Chen", city: "Vancouver", country: "CA" },
  { email: "carol.rodriguez@inbox.demo", name: "Carol Rodriguez", city: "Austin", country: "US" },
  { email: "david.smith@example.invalid", name: "David Smith", city: "London", country: "GB" },
  { email: "emma.johnson@test.local", name: "Emma Johnson", city: "Melbourne", country: "AU" },
];

const customerIds = [];
for (const customer of customers) {
  const result = db.run(
    "INSERT INTO customers (email, full_name, city, country) VALUES (?, ?, ?, ?)",
    [customer.email, customer.name, customer.city, customer.country]
  );
  customerIds.push(result.lastInsertRowid);
}

// Insert products
const products = [
  { sku: "WDG-2024-BLU", name: "Blue Widget", desc: "A sturdy blue widget for everyday use", price: 2499, stock: 45 },
  { sku: "WDG-2024-RED", name: "Red Widget", desc: "Premium red widget with enhanced features", price: 3499, stock: 23 },
  { sku: "GDG-PRO-001", name: "Professional Gadget", desc: "Top-tier gadget for professionals", price: 8999, stock: 12 },
  { sku: "SPK-BT-500", name: "Bluetooth Speaker", desc: "Portable wireless speaker with rich sound", price: 5999, stock: 67 },
  { sku: "USB-C-CABLE", name: "USB-C Cable 2m", desc: "Durable braided USB-C charging cable", price: 1299, stock: 150 },
  { sku: "MUG-CERAMIC", name: "Ceramic Mug", desc: "Hand-glazed ceramic mug, holds 350ml", price: 899, stock: 88 },
  { sku: "NOTE-PAD-A5", name: "A5 Notebook", desc: "Lined notebook with 120 pages", price: 599, stock: 200 },
];

const productIds = [];
for (const product of products) {
  const result = db.run(
    "INSERT INTO products (sku, name, description, price_cents, stock_quantity, active) VALUES (?, ?, ?, ?, ?, ?)",
    [product.sku, product.name, product.desc, product.price, product.stock, 1]
  );
  productIds.push(result.lastInsertRowid);
}

// Insert orders
const orders = [
  { customerId: customerIds[0], orderNum: "ORD-2024-001", status: "completed", items: [[productIds[0], 2], [productIds[4], 1]] },
  { customerId: customerIds[1], orderNum: "ORD-2024-002", status: "completed", items: [[productIds[2], 1]] },
  { customerId: customerIds[0], orderNum: "ORD-2024-003", status: "shipped", items: [[productIds[1], 1], [productIds[5], 2]] },
  { customerId: customerIds[2], orderNum: "ORD-2024-004", status: "processing", items: [[productIds[3], 1], [productIds[6], 3]] },
  { customerId: customerIds[3], orderNum: "ORD-2024-005", status: "pending", items: [[productIds[0], 5], [productIds[4], 2]] },
  { customerId: customerIds[4], orderNum: "ORD-2024-006", status: "completed", items: [[productIds[6], 5]] },
];

const orderIds = [];
for (const order of orders) {
  let total = 0;
  for (const [productId, quantity] of order.items) {
    const product = db.query("SELECT price_cents FROM products WHERE id = ?").get(productId) as { price_cents: number };
    total += product.price_cents * quantity;
  }
  
  const result = db.run(
    "INSERT INTO orders (customer_id, order_number, status, total_cents) VALUES (?, ?, ?, ?)",
    [order.customerId, order.orderNum, order.status, total]
  );
  const orderId = result.lastInsertRowid;
  orderIds.push(orderId);
  
  // Insert order items
  for (const [productId, quantity] of order.items) {
    const product = db.query("SELECT price_cents FROM products WHERE id = ?").get(productId) as { price_cents: number };
    db.run(
      "INSERT INTO order_items (order_id, product_id, quantity, price_cents) VALUES (?, ?, ?, ?)",
      [orderId, productId, quantity, product.price_cents]
    );
  }
}

// Insert attachments (polymorphic)
const attachments = [
  { type: "Customer", id: customerIds[0], filename: "profile_photo.jpg", size: 245670, mime: "image/jpeg" },
  { type: "Product", id: productIds[2], filename: "user_manual.pdf", size: 1024567, mime: "application/pdf" },
  { type: "Product", id: productIds[2], filename: "product_image.png", size: 567890, mime: "image/png" },
  { type: "Order", id: orderIds[1], filename: "invoice.pdf", size: 89456, mime: "application/pdf" },
  { type: "Customer", id: customerIds[3], filename: "id_verification.jpg", size: 334567, mime: "image/jpeg" },
];

for (const attachment of attachments) {
  db.run(
    "INSERT INTO attachments (attachable_type, attachable_id, filename, file_size, mime_type) VALUES (?, ?, ?, ?, ?)",
    [attachment.type, attachment.id, attachment.filename, attachment.size, attachment.mime]
  );
}

// Insert comments (polymorphic)
const comments = [
  { type: "Product", id: productIds[0], author: "Alice W.", body: "Great widget! Exactly what I needed." },
  { type: "Product", id: productIds[2], author: "Bob C.", body: "Works perfectly with my workflow." },
  { type: "Order", id: orderIds[0], author: "Support", body: "Order shipped via standard delivery." },
  { type: "Product", id: productIds[3], author: "Emma J.", body: "Sound quality is amazing!" },
  { type: "Order", id: orderIds[3], author: "Carol R.", body: "When will this ship?" },
];

for (const comment of comments) {
  db.run(
    "INSERT INTO comments (commentable_type, commentable_id, author_name, body) VALUES (?, ?, ?, ?)",
    [comment.type, comment.id, comment.author, comment.body]
  );
}

db.close();

console.log(`✓ Demo database created successfully at ${DB_PATH}`);
console.log(`  - ${customers.length} customers`);
console.log(`  - ${products.length} products`);
console.log(`  - ${orders.length} orders`);
console.log(`  - ${attachments.length} attachments (polymorphic)`);
console.log(`  - ${comments.length} comments (polymorphic)`);
console.log(`  - order_items.product_id deliberately has no foreign key`);
console.log("");
console.log(`Database path: ${DB_PATH}`);
