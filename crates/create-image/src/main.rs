use std::{
    env, fs,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    process,
};
use gpt::GptConfig;

const FAT32_PARTITION_SIZE: u64 = 512 * 1024 * 1024; // 512 MB
const SECTOR_SIZE: u64 = 512;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: create-image <kernel-elf> <output-img>");
        process::exit(1);
    }

    let kernel = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);

    if !kernel.exists() {
        eprintln!("Error: kernel ELF not found: {}", kernel.display());
        process::exit(1);
    }

    bootloader::UefiBoot::new(&kernel)
        .create_disk_image(&output)
        .unwrap_or_else(|e| {
            eprintln!(
                "Failed to create UEFI disk image from '{}' to '{}': {}",
                kernel.display(),
                output.display(),
                e
            );
            process::exit(1);
        });

    println!("Created UEFI disk image: {}", output.display());

    // Add FAT32 partition for storage testing on QEMU
    if let Err(e) = add_fat32_partition(&output) {
        eprintln!("Warning: could not add FAT32 partition: {}", e);
        eprintln!("The kernel will use RamFS as root instead of persistent FAT32 storage.");
    } else {
        println!("Added FAT32 storage partition to disk image for Phase 1 testing");
    }
}

fn add_fat32_partition(img_path: &PathBuf) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(img_path)?;

    // Get the current image size and extend it (sparsely) for the partition.
    let current_size = file.seek(SeekFrom::End(0))?;
    let new_size = current_size + FAT32_PARTITION_SIZE + 1024 * 1024;
    eprintln!(
        "[create-image] Extending disk from {} MB to {} MB...",
        current_size / 1024 / 1024,
        new_size / 1024 / 1024
    );
    file.set_len(new_size)?;
    file.sync_all()?;

    // Use the gpt crate with proper configuration for the extended disk
    eprintln!("[create-image] Updating GPT table...");
    
    let mut disk = GptConfig::new()
        .writable(true)
        .open(img_path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("GPT error on initial open: {}", e)))?;

    let existing_partitions = disk.partitions().clone();
    disk.update_partitions(existing_partitions).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to refresh GPT headers after resize: {}", e),
        )
    })?;

    let partition = gpt::partition_types::BASIC;
    let partition_id = disk
        .add_partition("rustos-storage", FAT32_PARTITION_SIZE, partition, 0, None)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to add partition: {}", e)))?;

    let part = disk
        .partitions()
        .get(&partition_id)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to locate newly created partition {}", partition_id),
            )
        })?;
    let part_start_sector = part.first_lba;
    let part_end_sector = part.last_lba;
    let total_sectors = new_size / SECTOR_SIZE;

    eprintln!("[create-image] Disk: {} sectors total", total_sectors);
    eprintln!(
        "[create-image] Partition 2: sectors {} to {}",
        part_start_sector, part_end_sector
    );

    disk.write()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to write GPT: {}", e)))?;

    // Format the partition as FAT32 labelled RUSTOS.
    eprintln!("[create-image] Formatting FAT32 filesystem...");
    let mut file = fs::OpenOptions::new().read(true).write(true).open(img_path)?;
    let total_part_sectors = part_end_sector - part_start_sector + 1;
    let base = part_start_sector * SECTOR_SIZE;
    let opts = fat_format::Options {
        label: "RUSTOS",
        serial: 0x5255_5354,
        fat_type: Some(fat_format::FatType::Fat32),
        hidden_sectors: part_start_sector as u32,
    };
    fat_format::format(total_part_sectors, &opts, |lba, sector| {
        file.seek(SeekFrom::Start(base + lba * SECTOR_SIZE))?;
        file.write_all(sector)
    })
    .map_err(|e| std::io::Error::other(format!("FAT32 format failed: {:?}", e)))?;
    file.sync_all()?;

    Ok(())
}


