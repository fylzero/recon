#!/usr/bin/env bun
/**
 * Demo database generator for Recon screenshot automation.
 * Creates a realistic SQLite database with:
 * - Rich e-commerce schema with 12-14 tables
 * - Polymorphic relationships (comments, attachments with *_type and *_id)
 * - Deliberately missing foreign keys
 * - Deterministic seeded random data (no real people or companies)
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

// Seeded deterministic random number generator
class SeededRandom {
  private seed: number;
  
  constructor(seed: number) {
    this.seed = seed;
  }
  
  next(): number {
    this.seed = (this.seed * 9301 + 49297) % 233280;
    return this.seed / 233280;
  }
  
  nextInt(min: number, max: number): number {
    return Math.floor(this.next() * (max - min + 1)) + min;
  }
  
  pick<T>(array: T[]): T {
    return array[Math.floor(this.next() * array.length)];
  }
}

const rng = new SeededRandom(42);

// Create schema
db.run(`
  CREATE TABLE customers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    full_name TEXT NOT NULL,
    phone TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

db.run(`
  CREATE TABLE addresses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id INTEGER NOT NULL,
    address_type TEXT NOT NULL,
    street_address TEXT NOT NULL,
    city TEXT NOT NULL,
    state TEXT,
    postal_code TEXT NOT NULL,
    country TEXT NOT NULL,
    is_default INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (customer_id) REFERENCES customers(id)
  )
`);

db.run(`
  CREATE INDEX idx_addresses_customer_id ON addresses(customer_id)
`);

db.run(`
  CREATE TABLE categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    slug TEXT NOT NULL UNIQUE,
    description TEXT,
    parent_category_id INTEGER,
    FOREIGN KEY (parent_category_id) REFERENCES categories(id)
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
  CREATE TABLE product_categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id INTEGER NOT NULL,
    category_id INTEGER NOT NULL,
    FOREIGN KEY (product_id) REFERENCES products(id),
    FOREIGN KEY (category_id) REFERENCES categories(id),
    UNIQUE(product_id, category_id)
  )
`);

db.run(`
  CREATE INDEX idx_product_categories_product ON product_categories(product_id)
`);

db.run(`
  CREATE INDEX idx_product_categories_category ON product_categories(category_id)
`);

db.run(`
  CREATE TABLE orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id INTEGER NOT NULL,
    order_number TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'pending',
    total_cents INTEGER NOT NULL,
    shipping_address_id INTEGER,
    notes TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (customer_id) REFERENCES customers(id),
    FOREIGN KEY (shipping_address_id) REFERENCES addresses(id)
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

db.run(`
  CREATE TABLE payments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id INTEGER NOT NULL,
    payment_method TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    status TEXT NOT NULL,
    transaction_id TEXT,
    processed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (order_id) REFERENCES orders(id)
  )
`);

db.run(`
  CREATE INDEX idx_payments_order_id ON payments(order_id)
`);

// Deliberately NO foreign key on order_id even though it references orders
db.run(`
  CREATE TABLE shipments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id INTEGER NOT NULL,
    tracking_number TEXT NOT NULL,
    carrier TEXT NOT NULL,
    status TEXT NOT NULL,
    shipped_at TEXT,
    delivered_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

db.run(`
  CREATE INDEX idx_shipments_order_id ON shipments(order_id)
`);

db.run(`
  CREATE TABLE reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id INTEGER NOT NULL,
    customer_id INTEGER NOT NULL,
    rating INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    verified_purchase INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (product_id) REFERENCES products(id),
    FOREIGN KEY (customer_id) REFERENCES customers(id)
  )
`);

db.run(`
  CREATE INDEX idx_reviews_product_id ON reviews(product_id)
`);

db.run(`
  CREATE INDEX idx_reviews_customer_id ON reviews(customer_id)
`);

db.run(`
  CREATE TABLE coupons (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    code TEXT NOT NULL UNIQUE,
    discount_percent INTEGER,
    discount_cents INTEGER,
    min_purchase_cents INTEGER,
    max_uses INTEGER,
    times_used INTEGER NOT NULL DEFAULT 0,
    expires_at TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )
`);

// Polymorphic table: comments can be on products, orders, or reviews
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

// Polymorphic table: attachments can belong to products, orders, or reviews
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

console.log("Schema created. Inserting demo data...");

// Seed data with deterministic random generation
const firstNames = ["Alice", "Bob", "Carol", "David", "Emma", "Frank", "Grace", "Henry", "Iris", "Jack", 
                    "Kate", "Leo", "Maya", "Noah", "Olivia", "Paul", "Quinn", "Riley", "Sam", "Tara",
                    "Uma", "Victor", "Wendy", "Xavier", "Yara", "Zoe", "Aiden", "Blake", "Chloe", "Dylan",
                    "Eva", "Felix", "Gwen", "Hugo", "Ivy", "Jesse", "Kai", "Luna", "Max", "Nina"];
const lastNames = ["Anderson", "Brown", "Chen", "Davis", "Evans", "Fisher", "Garcia", "Harris", "Irving", "Johnson",
                   "Klein", "Lopez", "Miller", "Nelson", "O'Brien", "Parker", "Quinn", "Roberts", "Smith", "Taylor",
                   "Underwood", "Valdez", "Wilson", "Xavier", "Young", "Zhang", "Abbott", "Blake", "Cooper", "Dixon"];
const cities = ["Portland", "Seattle", "Denver", "Austin", "Boston", "Chicago", "Phoenix", "Atlanta", "Miami", "Dallas"];
const states = ["OR", "WA", "CO", "TX", "MA", "IL", "AZ", "GA", "FL", "TX"];
const countries = ["US", "CA", "GB", "AU", "NZ"];
const streets = ["Main St", "Oak Ave", "Maple Dr", "Pine Ln", "Cedar Rd", "Elm St", "Park Pl", "Lake Dr", "River Rd", "Hill St"];

// Insert 50 customers
const customerIds = [];
for (let i = 0; i < 50; i++) {
  const firstName = rng.pick(firstNames);
  const lastName = rng.pick(lastNames);
  const email = `${firstName.toLowerCase()}.${lastName.toLowerCase()}${i > 0 ? i : ''}@example.test`;
  const phone = `555-${String(rng.nextInt(100, 999)).padStart(3, '0')}-${String(rng.nextInt(1000, 9999)).padStart(4, '0')}`;
  
  const result = db.run(
    "INSERT INTO customers (email, full_name, phone) VALUES (?, ?, ?)",
    [email, `${firstName} ${lastName}`, phone]
  );
  customerIds.push(result.lastInsertRowid);
}

// Insert addresses for customers
for (const customerId of customerIds) {
  const numAddresses = rng.nextInt(1, 2);
  for (let i = 0; i < numAddresses; i++) {
    const addrType = i === 0 ? "billing" : "shipping";
    const street = `${rng.nextInt(100, 9999)} ${rng.pick(streets)}`;
    const city = rng.pick(cities);
    const state = rng.pick(states);
    const postal = `${rng.nextInt(10000, 99999)}`;
    const country = rng.pick(countries);
    
    db.run(
      "INSERT INTO addresses (customer_id, address_type, street_address, city, state, postal_code, country, is_default) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
      [customerId, addrType, street, city, state, postal, country, i === 0 ? 1 : 0]
    );
  }
}

// Insert categories
const categories = [
  { name: "Electronics", slug: "electronics", desc: "Electronic devices and accessories" },
  { name: "Home & Garden", slug: "home-garden", desc: "Home improvement and garden supplies" },
  { name: "Clothing", slug: "clothing", desc: "Apparel and fashion items" },
  { name: "Books", slug: "books", desc: "Physical and digital books" },
  { name: "Sports", slug: "sports", desc: "Sports equipment and outdoor gear" },
  { name: "Toys", slug: "toys", desc: "Toys and games for all ages" },
];

const categoryIds = [];
for (const cat of categories) {
  const result = db.run(
    "INSERT INTO categories (name, slug, description) VALUES (?, ?, ?)",
    [cat.name, cat.slug, cat.desc]
  );
  categoryIds.push(result.lastInsertRowid);
}

// Insert 80 products
const productNames = [
  "Wireless Mouse", "Mechanical Keyboard", "USB Hub", "HDMI Cable", "Laptop Stand",
  "Desk Lamp", "Monitor Arm", "Cable Organizer", "Phone Holder", "Webcam",
  "Garden Hose", "Plant Pot", "Pruning Shears", "Watering Can", "Seed Starter Kit",
  "Cotton T-Shirt", "Denim Jeans", "Winter Jacket", "Running Shoes", "Baseball Cap",
  "Mystery Novel", "Science Fiction Anthology", "Cooking Guide", "History Atlas", "Poetry Collection",
  "Yoga Mat", "Resistance Bands", "Water Bottle", "Tennis Racket", "Bicycle Helmet",
  "Board Game", "Puzzle 1000pc", "Action Figure", "Building Blocks", "Card Game",
  "Portable Charger", "Bluetooth Speaker", "Earbuds", "Phone Case", "Screen Protector",
  "Coffee Maker", "Blender", "Toaster", "Kitchen Knife Set", "Cutting Board",
  "Backpack", "Travel Mug", "Umbrella", "Sunglasses", "Watch",
  "Notebook", "Pen Set", "Desk Organizer", "Stapler", "Paper Clips",
  "Pet Bed", "Dog Leash", "Cat Toy", "Fish Tank", "Bird Feeder",
  "Candle", "Picture Frame", "Throw Pillow", "Area Rug", "Wall Clock",
  "Toolbox", "Hammer", "Screwdriver Set", "Tape Measure", "Level",
  "Paint Brush Set", "Canvas Pack", "Sketch Pad", "Colored Pencils", "Markers",
  "Gaming Mouse", "Keyboard Wrist Rest", "Mouse Pad", "USB Cable", "External SSD"
];

const productIds = [];
for (let i = 0; i < 80; i++) {
  const name = productNames[i] || `Product ${i + 1}`;
  const sku = `SKU-${String(i + 1).padStart(4, '0')}`;
  const price = rng.nextInt(5, 200) * 100;
  const stock = rng.nextInt(0, 250);
  
  const result = db.run(
    "INSERT INTO products (sku, name, description, price_cents, stock_quantity, active) VALUES (?, ?, ?, ?, ?, ?)",
    [sku, name, `High-quality ${name.toLowerCase()} for everyday use`, price, stock, 1]
  );
  productIds.push(result.lastInsertRowid);
}

// Associate products with categories
for (const productId of productIds) {
  const numCategories = rng.nextInt(1, 2);
  const selectedCategories = new Set<number>();
  while (selectedCategories.size < numCategories) {
    selectedCategories.add(rng.pick(categoryIds));
  }
  for (const categoryId of selectedCategories) {
    db.run(
      "INSERT INTO product_categories (product_id, category_id) VALUES (?, ?)",
      [productId, categoryId]
    );
  }
}

// Insert coupons
const couponCodes = ["SAVE10", "WELCOME20", "FREESHIP", "SUMMER25", "FALL15"];
for (const code of couponCodes) {
  const discountPercent = code === "FREESHIP" ? null : rng.nextInt(10, 25);
  const discountCents = null;
  const minPurchase = rng.nextInt(2000, 5000);
  
  db.run(
    "INSERT INTO coupons (code, discount_percent, discount_cents, min_purchase_cents, max_uses, times_used, active) VALUES (?, ?, ?, ?, ?, ?, ?)",
    [code, discountPercent, discountCents, minPurchase, rng.nextInt(50, 200), rng.nextInt(0, 30), 1]
  );
}

// Insert 150 orders with multiple items each
const statuses = ["pending", "processing", "shipped", "delivered", "completed", "cancelled"];
const orderIds = [];
const addressIds = db.query("SELECT id FROM addresses").all() as { id: number }[];

for (let i = 0; i < 150; i++) {
  const customerId = rng.pick(customerIds);
  const orderNum = `ORD-2026-${String(i + 1).padStart(5, '0')}`;
  const status = rng.pick(statuses);
  const numItems = rng.nextInt(1, 5);
  
  // Pick shipping address
  const customerAddresses = addressIds.filter(a => {
    const addr = db.query("SELECT customer_id FROM addresses WHERE id = ?").get(a.id) as { customer_id: number };
    return addr.customer_id === customerId;
  });
  const shippingAddressId = customerAddresses.length > 0 ? rng.pick(customerAddresses).id : null;
  
  let total = 0;
  const items: Array<[number, number, number]> = [];
  for (let j = 0; j < numItems; j++) {
    const productId = rng.pick(productIds);
    const quantity = rng.nextInt(1, 3);
    const product = db.query("SELECT price_cents FROM products WHERE id = ?").get(productId) as { price_cents: number };
    total += product.price_cents * quantity;
    items.push([productId, quantity, product.price_cents]);
  }
  
  const result = db.run(
    "INSERT INTO orders (customer_id, order_number, status, total_cents, shipping_address_id) VALUES (?, ?, ?, ?, ?)",
    [customerId, orderNum, status, total, shippingAddressId]
  );
  const orderId = result.lastInsertRowid;
  orderIds.push(orderId);
  
  // Insert order items
  for (const [productId, quantity, price] of items) {
    db.run(
      "INSERT INTO order_items (order_id, product_id, quantity, price_cents) VALUES (?, ?, ?, ?)",
      [orderId, productId, quantity, price]
    );
  }
  
  // Insert payment for completed/shipped orders
  if (["completed", "shipped", "delivered"].includes(status)) {
    const methods = ["credit_card", "debit_card", "paypal", "apple_pay"];
    db.run(
      "INSERT INTO payments (order_id, payment_method, amount_cents, status, transaction_id) VALUES (?, ?, ?, ?, ?)",
      [orderId, rng.pick(methods), total, "completed", `TXN-${rng.nextInt(100000, 999999)}`]
    );
  }
  
  // Insert shipment for shipped orders
  if (["shipped", "delivered"].includes(status)) {
    const carriers = ["FedEx", "UPS", "USPS", "DHL"];
    db.run(
      "INSERT INTO shipments (order_id, tracking_number, carrier, status) VALUES (?, ?, ?, ?)",
      [orderId, `TRK${rng.nextInt(1000000000, 9999999999)}`, rng.pick(carriers), status === "delivered" ? "delivered" : "in_transit"]
    );
  }
}

// Insert reviews
const reviewTitles = [
  "Great product!", "Exactly what I needed", "Good value", "Highly recommend",
  "Not bad", "Could be better", "Amazing quality", "Worth the price",
  "Disappointed", "Excellent purchase", "Five stars", "Will buy again"
];

for (let i = 0; i < 60; i++) {
  const productId = rng.pick(productIds);
  const customerId = rng.pick(customerIds);
  const rating = rng.nextInt(1, 5);
  const title = rng.pick(reviewTitles);
  const body = rating >= 4 ? "Really happy with this purchase. Quality is excellent." : "It's okay but not what I expected.";
  
  db.run(
    "INSERT INTO reviews (product_id, customer_id, rating, title, body, verified_purchase) VALUES (?, ?, ?, ?, ?, ?)",
    [productId, customerId, rating, title, body, rng.nextInt(0, 1)]
  );
}

// Insert comments (polymorphic: Product, Order, Review)
const commentAuthors = ["Support Team", "Admin", "Customer Service", "Manager", "Staff"];
const reviewIds = db.query("SELECT id FROM reviews LIMIT 20").all() as { id: number }[];

for (let i = 0; i < 40; i++) {
  const typeChoice = rng.nextInt(1, 3);
  let type: string, id: number;
  
  if (typeChoice === 1) {
    type = "Product";
    id = rng.pick(productIds);
  } else if (typeChoice === 2) {
    type = "Order";
    id = rng.pick(orderIds);
  } else {
    type = "Review";
    id = rng.pick(reviewIds).id;
  }
  
  const bodies = [
    "Thank you for your feedback!",
    "We appreciate your business.",
    "Your order has been processed.",
    "Great to hear you're satisfied!",
    "We're looking into this issue."
  ];
  
  db.run(
    "INSERT INTO comments (commentable_type, commentable_id, author_name, body) VALUES (?, ?, ?, ?)",
    [type, id, rng.pick(commentAuthors), rng.pick(bodies)]
  );
}

// Insert attachments (polymorphic: Product, Order, Review)
const mimeTypes = ["image/jpeg", "image/png", "application/pdf", "image/gif"];
const filenames = ["product_image.jpg", "manual.pdf", "invoice.pdf", "photo.png", "diagram.jpg"];

for (let i = 0; i < 30; i++) {
  const typeChoice = rng.nextInt(1, 3);
  let type: string, id: number;
  
  if (typeChoice === 1) {
    type = "Product";
    id = rng.pick(productIds);
  } else if (typeChoice === 2) {
    type = "Order";
    id = rng.pick(orderIds);
  } else {
    type = "Review";
    id = rng.pick(reviewIds).id;
  }
  
  db.run(
    "INSERT INTO attachments (attachable_type, attachable_id, filename, file_size, mime_type) VALUES (?, ?, ?, ?, ?)",
    [type, id, rng.pick(filenames), rng.nextInt(50000, 2000000), rng.pick(mimeTypes)]
  );
}

db.close();

const tableNames = ["customers", "addresses", "categories", "products", "product_categories", "orders", 
                    "order_items", "payments", "shipments", "reviews", "coupons", "comments", "attachments"];
console.log(`✓ Demo database created successfully at ${DB_PATH}`);
console.log(`  - 13 tables: ${tableNames.join(", ")}`);
console.log(`  - 50 customers with addresses`);
console.log(`  - 80 products across 6 categories`);
console.log(`  - 150 orders with payments and shipments`);
console.log(`  - 60 reviews, 5 coupons`);
console.log(`  - 40 comments (polymorphic: Product, Order, Review)`);
console.log(`  - 30 attachments (polymorphic: Product, Order, Review)`);
console.log(`  - Missing FKs: order_items.product_id, shipments.order_id`);
console.log("");
console.log(`Database path: ${DB_PATH}`);
