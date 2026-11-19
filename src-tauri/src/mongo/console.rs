//! Reads console input: a command document such as `{ ping: 1 }`, or the
//! common mongosh forms, such as `db.users.find({ age: { $gt: 30 } }).limit(5)`.
//! It's a small subset of the shell, not JavaScript.

use mongodb::bson::{Bson, Document};

use super::literal::{type_name, Parser};

#[derive(Debug, PartialEq)]
pub enum Command {
    ShowDatabases,
    ShowCollections,
    Use(String),
    Run {
        admin: bool,
        command: Document,
    },
    CreateCollection {
        name: String,
        options: Document,
    },
    DropDatabase,
    CollectionNames,
    Find(Find),
    Count {
        coll: String,
        filter: Document,
    },
    EstimatedCount {
        coll: String,
    },
    Distinct {
        coll: String,
        key: String,
        filter: Document,
    },
    Aggregate {
        coll: String,
        pipeline: Vec<Document>,
    },
    InsertOne {
        coll: String,
        doc: Document,
    },
    InsertMany {
        coll: String,
        docs: Vec<Document>,
    },
    Update {
        coll: String,
        filter: Document,
        update: Bson,
        many: bool,
        upsert: bool,
    },
    Replace {
        coll: String,
        filter: Document,
        doc: Document,
        upsert: bool,
    },
    Delete {
        coll: String,
        filter: Document,
        many: bool,
    },
    CreateIndex {
        coll: String,
        keys: Document,
        options: Document,
    },
    DropIndex {
        coll: String,
        index: Bson,
    },
    Indexes {
        coll: String,
    },
    Drop {
        coll: String,
    },
}

#[derive(Debug, Default, PartialEq)]
pub struct Find {
    pub coll: String,
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub skip: Option<u64>,
    pub limit: Option<i64>,
    pub one: bool,
    pub count: bool,
}

impl Command {
    /// Whether the command can change data or collections, so the browser should refresh.
    pub fn changes(&self) -> bool {
        match self {
            Command::Run { .. }
            | Command::CreateCollection { .. }
            | Command::DropDatabase
            | Command::InsertOne { .. }
            | Command::InsertMany { .. }
            | Command::Update { .. }
            | Command::Replace { .. }
            | Command::Delete { .. }
            | Command::CreateIndex { .. }
            | Command::DropIndex { .. }
            | Command::Drop { .. } => true,
            Command::Aggregate { pipeline, .. } => pipeline
                .last()
                .is_some_and(|stage| stage.contains_key("$out") || stage.contains_key("$merge")),
            _ => false,
        }
    }
}

fn document(value: Option<Bson>, what: &str) -> Result<Document, String> {
    match value {
        None | Some(Bson::Null) => Ok(Document::new()),
        Some(Bson::Document(doc)) => Ok(doc),
        Some(other) => Err(format!(
            "The {what} is a document in {{ }}, not {}.",
            type_name(&other)
        )),
    }
}

fn string(value: Option<Bson>, what: &str) -> Result<String, String> {
    match value {
        Some(Bson::String(text)) => Ok(text),
        Some(other) => Err(format!(
            "The {what} is a string, not {}.",
            type_name(&other)
        )),
        None => Err(format!("Give the {what}.")),
    }
}

fn number(value: Option<Bson>, what: &str) -> Result<i64, String> {
    match value {
        Some(Bson::Int32(n)) => Ok(n.into()),
        Some(Bson::Int64(n)) => Ok(n),
        Some(Bson::Double(n)) if n.fract() == 0.0 => Ok(n as i64),
        _ => Err(format!("{what} takes a whole number.")),
    }
}

fn documents(value: Option<Bson>, what: &str) -> Result<Vec<Document>, String> {
    match value {
        Some(Bson::Array(items)) => items
            .into_iter()
            .map(|item| document(Some(item), what))
            .collect(),
        _ => Err(format!(
            "The {what}s go in an array, like [{{ … }}, {{ … }}]."
        )),
    }
}

fn upsert(options: Option<Bson>) -> Result<bool, String> {
    let options = document(options, "options")?;
    Ok(options.get_bool("upsert").unwrap_or(false))
}

pub fn parse(line: &str) -> Result<Command, String> {
    let text = line.trim().trim_end_matches(';').trim();
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        ["show", "dbs" | "databases"] => return Ok(Command::ShowDatabases),
        ["show", "collections" | "tables"] => return Ok(Command::ShowCollections),
        ["use", name] => return Ok(Command::Use((*name).to_string())),
        _ => {}
    }
    let mut parser = Parser::new(text);
    if parser.peek() == Some('{') {
        let command = document(Some(parser.value()?), "command")?;
        parser.end()?;
        return Ok(Command::Run {
            admin: false,
            command,
        });
    }
    if parser.identifier() != Some("db") {
        return Err(
            "Type a command document like { ping: 1 }, or a shell command like db.users.find() or show collections."
                .into(),
        );
    }
    let mut segments: Vec<String> = Vec::new();
    let method = loop {
        if parser.eat('[') {
            segments.push(parser.string()?);
            parser.expect(']')?;
            continue;
        }
        if !parser.eat('.') {
            return Err(parser.error("Expected a method call like .find()"));
        }
        let name = parser
            .identifier()
            .ok_or_else(|| parser.error("Expected a name"))?
            .to_string();
        if parser.peek() == Some('(') {
            if name == "getCollection" && segments.is_empty() {
                let mut args = parser.arguments()?;
                if args.len() != 1 {
                    return Err("getCollection() takes the collection name.".into());
                }
                segments.push(string(args.pop(), "collection name")?);
                continue;
            }
            break name;
        }
        segments.push(name);
    };
    let mut args = parser.arguments()?.into_iter();
    let command = if segments.is_empty() {
        database_method(&method, &mut args)?
    } else {
        let coll = segments.join(".");
        collection_method(coll, &method, &mut args, &mut parser)?
    };
    if args.next().is_some() {
        return Err(format!("{method}() got more arguments than it takes."));
    }
    parser.end()?;
    Ok(command)
}

fn database_method(method: &str, args: &mut impl Iterator<Item = Bson>) -> Result<Command, String> {
    Ok(match method {
        "runCommand" | "adminCommand" => {
            let command = match args.next() {
                Some(Bson::String(name)) => {
                    let mut doc = Document::new();
                    doc.insert(name, 1);
                    doc
                }
                other => document(other, "command")?,
            };
            if command.is_empty() {
                return Err("Give the command, like { ping: 1 }.".into());
            }
            Command::Run {
                admin: method == "adminCommand",
                command,
            }
        }
        "createCollection" => Command::CreateCollection {
            name: string(args.next(), "collection name")?,
            options: document(args.next(), "options")?,
        },
        "dropDatabase" => Command::DropDatabase,
        "getCollectionNames" => Command::CollectionNames,
        "stats" => Command::Run {
            admin: false,
            command: mongodb::bson::doc! { "dbStats": 1 },
        },
        "version" => Command::Run {
            admin: true,
            command: mongodb::bson::doc! { "buildInfo": 1 },
        },
        "getName" | "getSiblingDB" => {
            return Err("Switch databases with use name, or the database list.".into())
        }
        other => return Err(format!("db.{other}() isn't a command the console knows.")),
    })
}

fn collection_method(
    coll: String,
    method: &str,
    args: &mut impl Iterator<Item = Bson>,
    parser: &mut Parser,
) -> Result<Command, String> {
    let command = match method {
        "find" | "findOne" => {
            let mut find = Find {
                coll,
                filter: document(args.next(), "filter")?,
                projection: args
                    .next()
                    .map(|value| document(Some(value), "projection"))
                    .transpose()?,
                one: method == "findOne",
                ..Find::default()
            };
            if !find.one {
                cursor_modifiers(&mut find, parser)?;
            }
            Command::Find(find)
        }
        "countDocuments" | "count" => Command::Count {
            coll,
            filter: document(args.next(), "filter")?,
        },
        "estimatedDocumentCount" => Command::EstimatedCount { coll },
        "distinct" => Command::Distinct {
            coll,
            key: string(args.next(), "field name")?,
            filter: document(args.next(), "filter")?,
        },
        "aggregate" => {
            let pipeline = documents(args.next(), "pipeline stage")?;
            let _ = args.next();
            Command::Aggregate { coll, pipeline }
        }
        "insertOne" | "insert" => match args.next() {
            Some(Bson::Array(items)) => Command::InsertMany {
                coll,
                docs: documents(Some(Bson::Array(items)), "document")?,
            },
            other => Command::InsertOne {
                coll,
                doc: document(other, "document")?,
            },
        },
        "insertMany" => Command::InsertMany {
            coll,
            docs: documents(args.next(), "document")?,
        },
        "updateOne" | "updateMany" => {
            let filter = document(args.next(), "filter")?;
            let update = match args.next() {
                Some(value @ (Bson::Document(_) | Bson::Array(_))) => value,
                _ => return Err("Give the update, like { $set: { field: value } }.".into()),
            };
            Command::Update {
                coll,
                filter,
                update,
                many: method == "updateMany",
                upsert: upsert(args.next())?,
            }
        }
        "replaceOne" => Command::Replace {
            coll,
            filter: document(args.next(), "filter")?,
            doc: document(args.next(), "replacement")?,
            upsert: upsert(args.next())?,
        },
        "deleteOne" | "deleteMany" | "remove" => Command::Delete {
            coll,
            filter: document(args.next(), "filter")?,
            many: method != "deleteOne",
        },
        "createIndex" => Command::CreateIndex {
            coll,
            keys: document(args.next(), "index keys")?,
            options: document(args.next(), "options")?,
        },
        "dropIndex" => Command::DropIndex {
            coll,
            index: args.next().ok_or("Give the index name or its keys.")?,
        },
        "getIndexes" => Command::Indexes { coll },
        "drop" => Command::Drop { coll },
        other => {
            return Err(format!(
                "{other}() isn't a collection method the console knows."
            ))
        }
    };
    Ok(command)
}

fn cursor_modifiers(find: &mut Find, parser: &mut Parser) -> Result<(), String> {
    while parser.eat('.') {
        let name = parser
            .identifier()
            .ok_or_else(|| parser.error("Expected a method name"))?
            .to_string();
        let mut args = parser.arguments()?.into_iter();
        match name.as_str() {
            "sort" => find.sort = Some(document(args.next(), "sort")?),
            "projection" => find.projection = Some(document(args.next(), "projection")?),
            "limit" => find.limit = Some(number(args.next(), "limit()")?),
            "skip" => {
                let skip = number(args.next(), "skip()")?;
                find.skip = Some(u64::try_from(skip).map_err(|_| "skip() can't be negative.")?);
            }
            "count" | "itcount" => find.count = true,
            "pretty" | "toArray" => {}
            other => {
                return Err(format!(
                    ".{other}() isn't a cursor method the console knows."
                ))
            }
        }
        if args.next().is_some() {
            return Err(format!(".{name}() got more arguments than it takes."));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn reads_commands() {
        assert_eq!(parse("show dbs").unwrap(), Command::ShowDatabases);
        assert_eq!(parse("use shop").unwrap(), Command::Use("shop".into()));
        assert_eq!(
            parse("{ ping: 1 }").unwrap(),
            Command::Run {
                admin: false,
                command: doc! { "ping": 1 }
            }
        );
        assert_eq!(
            parse("db.adminCommand('listDatabases')").unwrap(),
            Command::Run {
                admin: true,
                command: doc! { "listDatabases": 1 }
            }
        );
    }

    #[test]
    fn reads_find_chains() {
        let Command::Find(find) = parse(
            "db.users.find({ age: { $gt: 30 } }, { name: 1 }).sort({ age: -1 }).skip(5).limit(10);",
        )
        .unwrap() else {
            panic!("not a find");
        };
        assert_eq!(find.coll, "users");
        assert_eq!(find.filter, doc! { "age": { "$gt": 30 } });
        assert_eq!(find.projection, Some(doc! { "name": 1 }));
        assert_eq!(find.sort, Some(doc! { "age": -1 }));
        assert_eq!(find.skip, Some(5));
        assert_eq!(find.limit, Some(10));
    }

    #[test]
    fn reads_dotted_and_quoted_collections() {
        let Command::Find(find) = parse("db.system.users.find()").unwrap() else {
            panic!()
        };
        assert_eq!(find.coll, "system.users");
        let Command::Count { coll, .. } =
            parse("db.getCollection('my orders').countDocuments()").unwrap()
        else {
            panic!()
        };
        assert_eq!(coll, "my orders");
        let Command::Drop { coll } = parse("db['a-b'].drop()").unwrap() else {
            panic!()
        };
        assert_eq!(coll, "a-b");
    }

    #[test]
    fn reads_writes() {
        assert!(matches!(
            parse("db.users.updateMany({}, { $set: { a: 1 } }, { upsert: true })").unwrap(),
            Command::Update {
                many: true,
                upsert: true,
                ..
            }
        ));
        assert!(matches!(
            parse("db.users.insertMany([{ a: 1 }, { a: 2 }])").unwrap(),
            Command::InsertMany { docs, .. } if docs.len() == 2
        ));
        assert!(parse("db.users.insertMany({ a: 1 })").is_err());
        assert!(parse("db.users.frobnicate()").is_err());
        assert!(parse("select * from users").is_err());
    }
}
