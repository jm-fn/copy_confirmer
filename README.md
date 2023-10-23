# Copy Confirmer

Command line tool that lets you check if one library is copied into other(s).

## Usage
> **Warning:**
> The crate is still pretty new and there are some big changes to the API to be expected.

### Example: Check with one destination directory
To check if directory __/path/to/source__ has been copied into __/path/to/destination__:
```
copcon -s /path/to/source -d /path/to/destination
```
If there are any files in the source dir missing in the destination (say, __/path/to/source/missing_file__ is missing from __/path/to/destination__), the program will print the missing files:
```diff
Missing files:
"/path/to/source/missing_file"
```

### Example: Check with multiple destinations
To check if __/path/to/source__ has been divided into two directories __/path/to/destination_1__ and __/path/to/destination_2__ run:
```
copcon -s /path/to/source -d /path/to/destination_1 -d /path/to/destination_2
```
If all files in the source directory are present in one of the destination directories, copy confirmer will print:
```
All files present in destinations
```
We can also print a json containing all files in source and their paths in destinations using flags ```--print-found --out-file some_file.json```.


### CLI options
```
Usage: copcon [OPTIONS] --source <SOURCE> --destination <DESTINATION>

Options:
  -s, --source <SOURCE>            Source directory
  -d, --destination <DESTINATION>  Destination directories
  -j, --jobs <JOBS>                Number of threads for checksum calculation [default: 1]
  -o, --out-file <OUT_FILE>        Print json output to this file
  -f, --print-found                Print json with all files found if copy is confirmed
  -h, --help                       Print help
  -V, --version                    Print version

```
